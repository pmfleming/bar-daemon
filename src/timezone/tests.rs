use super::{
    BUS, Cached, LOCATION, Observations, PATH, PublicStatus, Timing, monitor_connection,
    parse_accepted_location, parse_location_status, read_location, state_for_timezone,
};
use crate::{model::LocationSource, state::StateStore};
use chrono::{DateTime, Utc};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::time::sleep;

fn accepted_json(source: &str, latitude: f64) -> String {
    serde_json::json!({"timezone":"Europe/Dublin", "latitude":latitude, "longitude":-6.2,
        "verified_at":1000.0, "source":source, "accuracy_meters":26000.0})
    .to_string()
}

#[test]
fn metadata_is_forward_compatible_but_invalid_evidence_is_rejected() {
    for (source, expected) in [
        ("ip", LocationSource::Ip),
        ("non_ip", LocationSource::NonIp),
        ("", LocationSource::Unknown),
        ("unknown", LocationSource::Unknown),
        ("future_gps", LocationSource::Unknown),
    ] {
        assert_eq!(
            parse_accepted_location(&accepted_json(source, 53.3))
                .unwrap()
                .unwrap()
                .source,
            expected
        );
    }
    let mut legacy: serde_json::Value = serde_json::from_str(&accepted_json("ip", 53.3)).unwrap();
    legacy.as_object_mut().unwrap().remove("source");
    legacy.as_object_mut().unwrap().remove("accuracy_meters");
    let fix = parse_accepted_location(&legacy.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(fix.source, LocationSource::Unknown);
    assert_eq!(fix.accuracy_meters, None);
    for (field, value) in [
        ("latitude", serde_json::json!(91)),
        ("longitude", serde_json::json!(-181)),
        ("timezone", serde_json::json!("invalid")),
        ("verified_at", serde_json::json!(0)),
        ("accuracy_meters", serde_json::json!(-1)),
        ("source", serde_json::json!(123)),
    ] {
        let mut json: serde_json::Value =
            serde_json::from_str(&accepted_json("future", 53.3)).unwrap();
        json[field] = value;
        assert!(
            parse_accepted_location(&json.to_string()).is_err(),
            "{field}"
        );
    }
}

#[test]
fn public_status_is_bounded_and_revision_detects_same_outcome_updates() {
    let initial = parse_location_status(br#"{"message":"","accepted_revision":1}"#);
    let next = parse_location_status(br#"{"message":"","accepted_revision":2}"#);
    assert_ne!(initial, next);
    assert_eq!(initial.message, None);
    assert_eq!(
        parse_location_status(br#"{"message":"Sources disagree"}"#)
            .message
            .as_deref(),
        Some("Sources disagree")
    );
    assert_eq!(
        parse_location_status(br#"{"message":""}"#),
        PublicStatus::default()
    );
    for bytes in [b"broken".to_vec(), vec![b' '; 4097]] {
        assert_eq!(
            parse_location_status(&bytes).message.as_deref(),
            Some("Location status unavailable")
        );
    }
}

#[test]
fn cached_zone_recomputes_dst_and_fractional_offset_locally() {
    let before = "2026-03-29T00:59:59Z".parse::<DateTime<Utc>>().unwrap();
    let after = "2026-03-29T01:00:00Z".parse::<DateTime<Utc>>().unwrap();
    assert_eq!(
        state_for_timezone("Europe/Brussels", before)
            .unwrap()
            .utc_offset_seconds,
        3600
    );
    assert_eq!(
        state_for_timezone("Europe/Brussels", after)
            .unwrap()
            .utc_offset_seconds,
        7200
    );
    assert_eq!(
        state_for_timezone("Asia/Kathmandu", after)
            .unwrap()
            .utc_offset_seconds,
        20700
    );
}

#[tokio::test(start_paused = true)]
async fn cached_reader_is_lazy_retains_verified_data_and_resets_bounded_backoff() {
    let mut cached = Cached::new("verified");
    let base = Duration::from_secs(2);
    for seconds in [2, 4, 8, 16, 32, 64, 128, 256, 300, 300] {
        assert!(
            cached
                .refresh(async { Err(anyhow::anyhow!("offline")) }, base)
                .await
        );
        assert_eq!(cached.value, "verified");
        assert_eq!(cached.error.as_deref(), Some("offline"));
        assert!(!cached.pending && !cached.retry.ready());
        assert!(
            !cached
                .refresh(async { panic!("idle reader was polled") }, base)
                .await
        );
        let delay = Duration::from_secs(seconds);
        assert_eq!(
            cached.retry.due.unwrap() - tokio::time::Instant::now(),
            delay
        );
        tokio::time::advance(delay).await;
        cached.pending = cached.retry.ready();
        assert!(cached.pending);
    }
    assert!(cached.refresh(async { Ok("updated") }, base).await);
    assert_eq!(cached.value, "updated");
    assert!(cached.error.is_none() && cached.retry.due.is_none());
    assert_eq!(cached.retry.failures, 0);
}

#[test]
fn observation_projection_and_status_keep_availability_and_invalidation_distinct() {
    let mut observed = Observations {
        zone: Cached::new(Some("Europe/Dublin".into())),
        location: Cached::new((
            parse_accepted_location(&accepted_json("ip", 53.3)).unwrap(),
            None,
        )),
        status: PublicStatus {
            message: Some("Checking".into()),
            revision: Some(1),
        },
    };
    observed.zone.error = Some("timezone offline".into());
    observed.location.error = Some("location offline".into());
    let now = "2026-10-20T12:00:00Z".parse().unwrap();
    let state = observed.project(now);
    assert!(!state.available);
    assert_eq!(state.error.as_deref(), Some("timezone offline"));
    assert_eq!(state.accepted_location.unwrap().latitude, 53.3);
    observed.zone.error = None;
    let state = observed.project(now);
    assert!(state.available);
    assert_eq!(state.error.as_deref(), Some("Checking"));
    observed.location.pending = false;
    assert!(observed.update_status(PublicStatus {
        message: None,
        revision: Some(1)
    }));
    assert!(!observed.location.pending);
    assert!(observed.update_status(PublicStatus {
        message: None,
        revision: Some(2)
    }));
    assert!(observed.location.pending);
    assert!(!observed.update_status(observed.status.clone()));
    assert!(observed.location.pending); // Presentation must not cancel a pending read.
    observed.location.pending = false;
    assert!(observed.update_status(PublicStatus::default()));
    assert!(observed.location.pending); // Legacy status without a revision still invalidates.
}

#[derive(Default)]
struct Fixture {
    zone: Mutex<String>,
    accepted: Mutex<String>,
    zone_reads: AtomicUsize,
    accepted_reads: AtomicUsize,
    zone_fails: AtomicBool,
    accepted_fails: AtomicBool,
    control_revision: AtomicUsize,
    control_changes: AtomicBool,
}
struct TimeService(Arc<Fixture>);
#[zbus::interface(name = "org.freedesktop.timedate1")]
impl TimeService {
    #[zbus(property)]
    fn timezone(&self) -> zbus::fdo::Result<String> {
        self.0.zone_reads.fetch_add(1, Ordering::SeqCst);
        if self.0.zone_fails.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed("temporary failure".into()));
        }
        Ok(self.0.zone.lock().unwrap().clone())
    }
}
struct LocationService(Arc<Fixture>);
#[zbus::interface(name = "org.laufan.LocationTimezone")]
impl LocationService {
    fn get_control(&self) -> zbus::fdo::Result<String> {
        let revision = self.0.control_revision.fetch_add(
            usize::from(self.0.control_changes.load(Ordering::SeqCst)),
            Ordering::SeqCst,
        );
        if revision == 0 {
            return Err(zbus::fdo::Error::UnknownMethod("legacy service".into()));
        }
        Ok(serde_json::json!({
            "available": true, "revision": revision,
            "manual": false, "paused": false, "uncertain": false,
        })
        .to_string())
    }
    fn get_accepted(&self) -> zbus::fdo::Result<String> {
        self.0.accepted_reads.fetch_add(1, Ordering::SeqCst);
        if self.0.accepted_fails.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed("temporary failure".into()));
        }
        Ok(self.0.accepted.lock().unwrap().clone())
    }
}

async fn wait_for(store: &StateStore, expected_zone: &str, latitude: f64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if store
                .read(|s| {
                    s.timezone.available
                        && s.timezone.timezone == expected_zone
                        && s.timezone
                            .accepted_location
                            .as_ref()
                            .is_some_and(|a| a.latitude == latitude)
                })
                .await
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn changed(server: &zbus::Connection) {
    server
        .emit_signal(
            None::<&str>,
            PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(
                BUS,
                std::collections::HashMap::from([("Timezone", zvariant::Value::from("changed"))]),
                Vec::<String>::new(),
            ),
        )
        .await
        .unwrap();
}
async fn status(path: &Path, revision: u64, message: &str) {
    // Use the same atomic replacement shape as the production writer.
    let next = path.with_extension("next");
    tokio::fs::write(
        &next,
        serde_json::json!({"message":message,"accepted_revision":revision}).to_string(),
    )
    .await
    .unwrap();
    tokio::fs::rename(next, path).await.unwrap();
}

#[tokio::test]
async fn private_bus_monitor_is_idle_locally_and_recovers_updates_failures_and_owner_changes() {
    use tokio::io::AsyncBufReadExt;
    let mut daemon = tokio::process::Command::new("dbus-daemon")
        .args([
            concat!(
                "--config-file=",
                env!("CARGO_MANIFEST_DIR"),
                "/test_support/location-session.conf"
            ),
            "--nofork",
            "--print-address=1",
        ])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let address = tokio::io::BufReader::new(daemon.stdout.take().unwrap())
        .lines()
        .next_line()
        .await
        .unwrap()
        .unwrap();
    let fixture = Arc::new(Fixture::default());
    *fixture.zone.lock().unwrap() = "Europe/Dublin".into();
    *fixture.accepted.lock().unwrap() = accepted_json("ip", 53.3);
    let server = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .name(BUS)
        .unwrap()
        .name(LOCATION)
        .unwrap()
        .serve_at(PATH, TimeService(fixture.clone()))
        .unwrap()
        .serve_at(
            "/org/laufan/LocationTimezone",
            LocationService(fixture.clone()),
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let connection = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("status.json");
    status(&path, 1, "").await;
    let store = StateStore::default();
    let monitor = {
        let store = store.clone();
        let path = path.clone();
        tokio::spawn(async move {
            monitor_connection(
                &connection,
                &store,
                &path,
                Timing {
                    status: Duration::from_millis(15),
                    local: Duration::from_millis(25),
                    retry: Duration::from_millis(30),
                },
            )
            .await
        })
    };
    wait_for(&store, "Europe/Dublin", 53.3).await;
    sleep(Duration::from_millis(50)).await;
    let counts = (
        fixture.zone_reads.load(Ordering::SeqCst),
        fixture.accepted_reads.load(Ordering::SeqCst),
    );
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        (
            fixture.zone_reads.load(Ordering::SeqCst),
            fixture.accepted_reads.load(Ordering::SeqCst)
        ),
        counts
    );
    // A message-only update is presentation, not a reason to re-authorize/read.
    status(&path, 1, "Pending confirmation").await;
    sleep(Duration::from_millis(50)).await;
    assert_eq!(fixture.accepted_reads.load(Ordering::SeqCst), counts.1);
    // Same outcome, same timezone, new accepted coordinates cannot be missed.
    *fixture.accepted.lock().unwrap() = accepted_json("future_gps", 54.0);
    status(&path, 2, "Pending confirmation").await;
    wait_for(&store, "Europe/Dublin", 54.0).await;
    assert_eq!(
        store
            .read(|s| s.timezone.accepted_location.as_ref().unwrap().source)
            .await,
        LocationSource::Unknown
    );
    assert_eq!(fixture.zone_reads.load(Ordering::SeqCst), counts.0);
    *fixture.zone.lock().unwrap() = "Asia/Kathmandu".into();
    changed(&server).await;
    wait_for(&store, "Asia/Kathmandu", 54.0).await;
    // Read failures retain accepted data and retry even without another revision.
    fixture.accepted_fails.store(true, Ordering::SeqCst);
    status(&path, 3, "").await;
    sleep(Duration::from_millis(60)).await;
    assert_eq!(
        store
            .read(|s| s.timezone.accepted_location.as_ref().unwrap().latitude)
            .await,
        54.0
    );
    *fixture.accepted.lock().unwrap() = accepted_json("non_ip", 55.0);
    fixture.accepted_fails.store(false, Ordering::SeqCst);
    wait_for(&store, "Asia/Kathmandu", 55.0).await;
    fixture.zone_fails.store(true, Ordering::SeqCst);
    changed(&server).await;
    sleep(Duration::from_millis(50)).await;
    assert!(!store.read(|s| s.timezone.available).await);
    fixture.zone_fails.store(false, Ordering::SeqCst);
    wait_for(&store, "Asia/Kathmandu", 55.0).await;
    // timedated may exit normally; no poll should reactivate it.
    server.release_name(BUS).await.unwrap();
    let reads = fixture.zone_reads.load(Ordering::SeqCst);
    sleep(Duration::from_millis(60)).await;
    assert_eq!(fixture.zone_reads.load(Ordering::SeqCst), reads);
    *fixture.zone.lock().unwrap() = "Europe/Brussels".into();
    server.request_name(BUS).await.unwrap();
    wait_for(&store, "Europe/Brussels", 55.0).await;
    server.release_name(LOCATION).await.unwrap();
    sleep(Duration::from_millis(30)).await;
    *fixture.accepted.lock().unwrap() = accepted_json("ip", 56.0);
    server.request_name(LOCATION).await.unwrap();
    wait_for(&store, "Europe/Brussels", 56.0).await;
    monitor.abort();
    let _ = monitor.await;
    fixture.control_revision.store(1, Ordering::SeqCst);
    let (accepted, control) = read_location(&server).await.unwrap();
    assert_eq!(accepted.unwrap().latitude, 56.0);
    assert_eq!(control.unwrap().revision, 1);
    fixture.control_changes.store(true, Ordering::SeqCst);
    assert_eq!(
        read_location(&server).await.unwrap_err().to_string(),
        "Location changed during read"
    );
}
