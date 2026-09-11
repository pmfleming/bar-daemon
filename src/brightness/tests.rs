use std::{fs, os::unix::fs::PermissionsExt, pin::pin};

use tempfile::{TempDir, tempdir};

use super::*;

fn device(root: &Path, name: &str, current: u64, maximum: u64) -> PathBuf {
    let path = root.join(name);
    fs::create_dir(&path).unwrap();
    for (file, value) in [
        ("brightness", current),
        ("actual_brightness", current),
        ("max_brightness", maximum),
    ] {
        fs::write(path.join(file), value.to_string()).unwrap();
    }
    path
}

fn fixture(current: u64, maximum: u64) -> (TempDir, BrightnessService, PathBuf) {
    let root = tempdir().unwrap();
    // Deliberately matches a real device name: isolation must depend on the root.
    let path = device(root.path(), "amdgpu_bl1", current, maximum);
    let service = BrightnessService::with_root(StateStore::default(), root.path().into());
    (root, service, path)
}

fn helper(root: &Path, body: &str) -> PathBuf {
    let shell = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("sh"))
        .find(|path| path.is_file())
        .unwrap();
    let path = root.join("fake-brightnessctl");
    fs::write(&path, format!("#!{}\nset -eu\n{body}\n", shell.display())).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn fail_direct_write(path: &Path) {
    fs::remove_file(path.join("brightness")).unwrap();
    fs::create_dir(path.join("brightness")).unwrap();
}

#[test]
fn discovers_highest_resolution_backlight() {
    let root = tempdir().unwrap();
    for (name, current, maximum) in [("small", 5, 10), ("panel", 600, 1000)] {
        let path = device(root.path(), name, current, maximum);
        fs::remove_file(path.join("brightness")).unwrap();
    }
    let device = discover(root.path()).unwrap();
    assert_eq!(device.name, "panel");
    assert_eq!(device.state().percent, 60);
}

#[test]
fn prefers_requested_brightness_over_hardware_feedback() {
    let (_root, service, path) = fixture(600, 1000);
    fs::write(path.join("actual_brightness"), "550").unwrap();
    let state = discover(&service.root).unwrap().state();
    assert_eq!(state.brightness, 600);
    assert_eq!(state.percent, 60);
}

#[test]
fn rounds_and_bounds_raw_values() {
    for maximum in [100, 255, 1000, 64764, u64::MAX] {
        for requested in 1..=100 {
            let raw = raw_brightness(maximum, requested);
            assert!((1..=maximum).contains(&raw));
            assert_eq!(percent(raw, maximum), requested);
        }
    }
    assert_eq!(raw_brightness(64764, 95), 61526);
    assert_eq!(raw_brightness(3, 1), 1);
    assert_eq!(percent(1, 3), 33);
    assert_eq!(percent(2, 3), 67);
    assert_eq!(percent(u64::MAX, 1), 100);
    assert_eq!(raw_brightness(0, 50), 0);
    assert_eq!(percent(1, 0), 0);
}

#[tokio::test]
async fn repeated_steps_use_requested_values_and_publish_readback() {
    let (_root, service, path) = fixture(64764, 64764);
    let mut events = service.state.subscribe();
    let downward = (5..=95).step_by(5).rev().map(|percent| (-5, percent));
    let upward = (10..=100).step_by(5).map(|percent| (5, percent));
    for (delta, expected) in downward.chain(upward) {
        let state = service.adjust(delta).await.unwrap();
        assert_eq!(state.percent, expected);
        assert_eq!(
            read_u64(&path.join("brightness")).unwrap(),
            raw_brightness(64764, expected)
        );
        assert_eq!(service.state.snapshot().await.brightness, state);
        assert_eq!(events.recv().await.unwrap().data["percent"], expected);
    }
    // The fixture's actual_brightness deliberately remains at 100% throughout.
    assert_eq!(read_u64(&path.join("actual_brightness")).unwrap(), 64764);
}

#[tokio::test]
async fn bounds_and_validation_do_not_turn_off_the_panel() {
    let (_root, service, path) = fixture(500, 1000);
    assert_eq!(service.adjust(i16::MIN).await.unwrap().percent, 1);
    assert_eq!(service.adjust(-5).await.unwrap().brightness, 10);
    assert_eq!(service.adjust(i16::MAX).await.unwrap().percent, 100);
    assert_eq!(service.adjust(5).await.unwrap().brightness, 1000);
    assert_eq!(service.set(1).await.unwrap().brightness, 10);
    for invalid in [0, 101, 255] {
        assert!(service.set(invalid).await.is_err());
        assert_eq!(read_u64(&path.join("brightness")).unwrap(), 10);
    }
}

#[tokio::test]
async fn explicit_root_is_shared_by_monitor_set_and_adjust() {
    let (_root, service, path) = fixture(500, 1000);
    assert!(service.fallback.is_none());
    service.refresh().await;
    assert_eq!(service.state.snapshot().await.brightness.percent, 50);
    assert_eq!(service.set(70).await.unwrap().brightness, 700);
    assert_eq!(service.adjust(-5).await.unwrap().brightness, 650);
    service.refresh().await;
    assert_eq!(service.state.snapshot().await.brightness.percent, 65);
    fail_direct_write(&path);
    let error = service.set(80).await.unwrap_err().to_string();
    assert!(
        error.contains("fallback disabled for explicit backlight root"),
        "{error}"
    );
    assert_eq!(service.state.snapshot().await.brightness.percent, 65);
}

#[tokio::test]
async fn permission_failure_is_reported_without_hardware_fallback() {
    let (_root, service, path) = fixture(500, 1000);
    let brightness = path.join("brightness");
    fs::set_permissions(&brightness, fs::Permissions::from_mode(0o444)).unwrap();
    // Root can bypass mode bits; deterministic write-failure coverage above
    // still exercises isolation when the suite is run as root.
    if fs::OpenOptions::new().write(true).open(&brightness).is_ok() {
        return;
    }
    let error = service.set(80).await.unwrap_err().to_string();
    assert!(error.contains("Permission denied"), "{error}");
    assert!(error.contains("fallback disabled"), "{error}");
    assert_eq!(read_u64(&brightness).unwrap(), 500);
}

#[tokio::test]
async fn fallback_receives_raw_target_and_returns_verified_state() {
    let (root, mut service, path) = fixture(500, 1000);
    fail_direct_write(&path);
    service.fallback = Some(helper(
        root.path(),
        r#"
[ "$1" = --device ] && [ "$2" = amdgpu_bl1 ] && [ "$3" = set ]
[ "$4" = 650 ] && [ "$5" = --quiet ] && [ "$#" = 5 ]
base=${0%/*}
rmdir "$base/amdgpu_bl1/brightness"
printf '%s' "$4" > "$base/amdgpu_bl1/brightness"
"#,
    ));
    let state = service.set(65).await.unwrap();
    assert_eq!(state.percent, 65);
    assert_eq!(service.state.snapshot().await.brightness, state);
}

#[tokio::test]
async fn fallback_errors_preserve_state_and_release_the_gate() {
    let (root, mut service, path) = fixture(500, 1000);
    service.refresh().await;
    fail_direct_write(&path);
    service.fallback = Some(helper(
        root.path(),
        "echo 'Permission denied by logind' >&2; exit 1",
    ));
    let error = service.set(65).await.unwrap_err().to_string();
    assert!(error.contains("brightnessctl failed"), "{error}");
    assert!(error.contains("Permission denied by logind"), "{error}");
    assert_eq!(service.state.snapshot().await.brightness.percent, 50);
    service.fallback = Some(root.path().join("missing-helper"));
    assert!(
        service
            .set(65)
            .await
            .unwrap_err()
            .to_string()
            .contains("start brightnessctl")
    );
    fs::remove_dir(path.join("brightness")).unwrap();
    fs::write(path.join("brightness"), "500").unwrap();
    assert_eq!(
        timeout(Duration::from_secs(1), service.adjust(5))
            .await
            .unwrap()
            .unwrap()
            .percent,
        55
    );
}

#[tokio::test]
async fn timed_out_helper_is_killed_and_next_operation_can_proceed() {
    let (root, mut service, path) = fixture(500, 1000);
    fail_direct_write(&path);
    service.fallback = Some(helper(
        root.path(),
        r#"
base=${0%/*}
printf '%s' "$$" > "$base/pid"
exec sleep 30
"#,
    ));
    let error = timeout(Duration::from_secs(5), service.set(65))
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("brightnessctl timed out"));
    let pid = fs::read_to_string(root.path().join("pid")).unwrap();
    timeout(Duration::from_secs(2), async {
        while Path::new(&format!("/proc/{pid}")).exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed-out child must be killed and reaped");
    fs::remove_dir(path.join("brightness")).unwrap();
    fs::write(path.join("brightness"), "500").unwrap();
    assert_eq!(
        timeout(Duration::from_secs(1), service.adjust(5))
            .await
            .unwrap()
            .unwrap()
            .percent,
        55
    );
}

#[tokio::test]
async fn refresh_waits_for_the_gate_before_reading() {
    let (_root, service, path) = fixture(500, 1000);
    let guard = service.gate.lock().await;
    let mut refresh = pin!(service.refresh());
    assert!(
        timeout(Duration::from_millis(30), &mut refresh)
            .await
            .is_err()
    );
    fs::write(path.join("brightness"), "700").unwrap();
    drop(guard);
    refresh.await;
    assert_eq!(service.state.snapshot().await.brightness.percent, 70);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_adjustments_and_refreshes_publish_monotonically() {
    let (_root, service, _path) = fixture(100, 1000);
    service.refresh().await;
    let mut events = service.state.subscribe();
    let mut tasks = Vec::new();
    for _ in 0..15 {
        let adjuster = service.clone();
        tasks.push(tokio::spawn(async move {
            adjuster.adjust(5).await.unwrap();
        }));
        let observer = service.clone();
        tasks.push(tokio::spawn(async move {
            observer.refresh().await;
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let mut previous = 10;
    let mut count = 0;
    while let Ok(event) = events.try_recv() {
        let current = event.data["percent"].as_u64().unwrap();
        assert_eq!(
            current,
            previous + 5,
            "no stale publication or lost key repeat"
        );
        previous = current;
        count += 1;
    }
    assert_eq!(count, 15);
    assert_eq!(service.state.snapshot().await.brightness.percent, 85);
}
