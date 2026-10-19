use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tempfile::TempDir;

fn network(id: &str) -> Network {
    Network {
        owner: ":1.2".into(),
        active: vec![id.into()],
        primary: id.into(),
    }
}
pub(super) fn fix(source: Source) -> Fix {
    Fix {
        latitude: 53.3498,
        longitude: -6.2603,
        timestamp: wall_time(),
        source,
        accuracy: 1000.0,
    }
}
fn zones() -> Zones {
    Zones::from_table("L Europe/Dublin Eire\n")
}
struct Fake {
    network: Mutex<Network>,
    zone: Mutex<String>,
    geo: Mutex<Fix>,
    ip_fix: Mutex<Fix>,
    geo_failed: AtomicBool,
    geo_changes_network: AtomicBool,
    pending_geo: AtomicBool,
    geo_live: AtomicBool,
    geo_calls: AtomicUsize,
    pending_ip: AtomicBool,
    ip_live: AtomicBool,
    ip_calls: AtomicUsize,
    ip_unavailable: AtomicBool,
    limited: AtomicBool,
    cancel_on_ip: Mutex<Option<watch::Sender<Control>>>,
    calls: AtomicUsize,
    lost_reply: AtomicBool,
    wrong_readback: AtomicBool,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            network: Mutex::new(network("a")),
            zone: Mutex::new("America/Edmonton".into()),
            geo: Mutex::new(fix(Source::NonIp)),
            ip_fix: Mutex::new(fix(Source::Ip)),
            geo_failed: AtomicBool::new(false),
            geo_changes_network: AtomicBool::new(false),
            pending_geo: AtomicBool::new(false),
            geo_live: AtomicBool::new(false),
            geo_calls: AtomicUsize::new(0),
            pending_ip: AtomicBool::new(false),
            ip_live: AtomicBool::new(false),
            ip_calls: AtomicUsize::new(0),
            ip_unavailable: AtomicBool::new(false),
            limited: AtomicBool::new(false),
            cancel_on_ip: Mutex::new(None),
            calls: AtomicUsize::new(0),
            lost_reply: AtomicBool::new(false),
            wrong_readback: AtomicBool::new(false),
        }
    }
}
struct Live<'a>(&'a AtomicBool);
impl Drop for Live<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
impl Backend for Fake {
    async fn observe(&self) -> Result<Observation> {
        Ok(Observation {
            network: Some(self.network.lock().unwrap().clone()),
            sleeping: false,
        })
    }
    async fn timezone(&self) -> Result<String> {
        Ok(self.zone.lock().unwrap().clone())
    }
    async fn set_timezone(&self, zone: &str) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.wrong_readback.load(Ordering::SeqCst) {
            *self.zone.lock().unwrap() = zone.into();
        }
        anyhow::ensure!(
            !self.lost_reply.load(Ordering::SeqCst),
            "reply lost after applying"
        );
        Ok(())
    }
    async fn geoclue(&self, _: f64) -> Result<Fix> {
        self.geo_calls.fetch_add(1, Ordering::SeqCst);
        self.geo_live.store(true, Ordering::SeqCst);
        let _live = Live(&self.geo_live);
        if self.pending_geo.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        anyhow::ensure!(
            !self.geo_failed.load(Ordering::SeqCst),
            "GeoClue unavailable"
        );
        if self.geo_changes_network.load(Ordering::SeqCst) {
            *self.network.lock().unwrap() = network("changed");
        }
        let mut fix = self.geo.lock().unwrap().clone();
        fix.timestamp = wall_time();
        Ok(fix)
    }
    async fn ip(&self) -> http::LookupResult {
        assert!(self.geo_calls.load(Ordering::SeqCst) > 0);
        assert!(
            !self.geo_live.load(Ordering::SeqCst),
            "IP must follow GeoClue"
        );
        self.ip_calls.fetch_add(1, Ordering::SeqCst);
        self.ip_live.store(true, Ordering::SeqCst);
        let _live = Live(&self.ip_live);
        if self.pending_ip.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if let Some(sender) = self.cancel_on_ip.lock().unwrap().as_ref() {
            sender.send_modify(|c| {
                c.epoch += 1;
                c.shutdown = true;
            });
        }
        if self.limited.load(Ordering::SeqCst) {
            return http::LookupResult::Limited(Duration::from_secs(7200));
        }
        if self.ip_unavailable.load(Ordering::SeqCst) {
            return http::LookupResult::Unavailable;
        }
        let mut fix = self.ip_fix.lock().unwrap().clone();
        fix.timestamp = wall_time();
        http::LookupResult::Fix(fix)
    }
}
pub(super) async fn authority(backend: &impl Backend) -> (TempDir, Authority) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::new(dir.path().join("state"), dir.path().join("run"));
    let mut a = Authority::new(storage, zones(), backend).await.unwrap();
    a.schedule.observe(Some(network("a")), Instant::now());
    (dir, a)
}
async fn check(a: &mut Authority, backend: &impl Backend) {
    let (_sender, mut control) = watch::channel(Control::default());
    a.check(backend, &mut control).await.unwrap();
}

#[test]
fn policy_accepts_ip_and_compares_zones_not_distance_accuracy_or_offset() {
    let z = zones();
    let now = wall_time();
    let start = now - 1.0;
    let mut geo = fix(Source::Ip);
    geo.accuracy = 300_000.0;
    assert_eq!(z.decide(&geo, None, start, now).unwrap(), "Europe/Dublin");
    for source in [Source::NonIp, Source::Unknown] {
        geo.source = source;
        assert!(matches!(
            z.decide(&geo, None, start, now),
            Err(Decision::Retry(_))
        ));
        assert_eq!(
            z.decide(&geo, Some(&fix(Source::Ip)), start, now).unwrap(),
            "Europe/Dublin"
        );
        // Edmonton and Calgary: ~280km apart in America/Edmonton.
        geo.latitude = 53.5461;
        geo.longitude = -113.4938;
        let mut ip = fix(Source::Ip);
        ip.latitude = 51.0447;
        ip.longitude = -114.0719;
        assert_eq!(
            z.decide(&geo, Some(&ip), start, now).unwrap(),
            "America/Edmonton"
        );
        // Derry and Letterkenny: <50km, opposite sides of a timezone boundary.
        geo.latitude = 54.9966;
        geo.longitude = -7.3086;
        ip.latitude = 54.95;
        ip.longitude = -7.7333;
        assert_eq!(z.geoclue_zone(&geo, start, now).unwrap(), "Europe/London");
        assert!(matches!(
            z.decide(&geo, Some(&ip), start, now),
            Err(Decision::Stop(_))
        ));
        // London and Dublin share offsets today but are not the same IANA zone.
        geo.latitude = 51.5074;
        geo.longitude = -0.1278;
        assert!(matches!(
            z.decide(&geo, Some(&fix(Source::Ip)), start, now),
            Err(Decision::Stop(_))
        ));
        geo = fix(Source::Ip);
    }
    assert_eq!(z.canonical("Eire"), z.canonical("Europe/Dublin"));
    assert_ne!(z.canonical("Europe/London"), z.canonical("Europe/Dublin"));
}

#[test]
fn invalid_stale_future_and_unresolved_evidence_are_retry_not_disagreement() {
    let z = zones();
    let now = wall_time();
    let start = now - 1.0;
    for source in [Source::Ip, Source::NonIp, Source::Unknown] {
        for scenario in [
            "old",
            "future",
            "nan",
            "negative_accuracy",
            "latitude",
            "longitude",
            "boundary",
        ] {
            let mut geo = fix(source);
            match scenario {
                "old" => geo.timestamp = start.floor() - 1.0,
                "future" => geo.timestamp = now + 6.0,
                "nan" => geo.accuracy = f64::NAN,
                "negative_accuracy" => geo.accuracy = -1.0,
                "latitude" => geo.latitude = 91.0,
                "longitude" => geo.longitude = f64::INFINITY,
                _ => {
                    geo.latitude = 90.0;
                    geo.longitude = 180.0;
                }
            }
            assert!(
                matches!(
                    z.decide(&geo, Some(&fix(Source::Ip)), start, now),
                    Err(Decision::Retry(_))
                ),
                "{scenario}"
            );
        }
    }
    let mut geo = fix(Source::NonIp);
    geo.timestamp = start.floor(); // GeoClue whole-second quantisation.
    assert!(z.decide(&geo, Some(&fix(Source::Ip)), start, now).is_ok());
    assert!(matches!(
        z.decide(&geo, Some(&fix(Source::Ip)), start, now + 301.0),
        Err(Decision::Retry(_))
    ));
    let mut ip = fix(Source::Ip);
    ip.timestamp = geo.timestamp - 121.0;
    assert!(matches!(
        z.decide(&geo, Some(&ip), start - 130.0, now),
        Err(Decision::Retry(_))
    ));
    ip = fix(Source::Unknown);
    assert!(matches!(
        z.decide(&geo, Some(&ip), start, now),
        Err(Decision::Retry(_))
    ));
}

#[test]
fn recent_evidence_is_reusable_within_network_epoch_but_not_across_it() {
    let now = wall_time();
    let mut geo = fix(Source::NonIp);
    geo.timestamp = now - 30.0;
    assert!(
        zones()
            .decide(&geo, Some(&fix(Source::Ip)), now - 60.0, now)
            .is_ok()
    );
    assert!(!geo.usable_geoclue(now - 10.0, now));
    geo.timestamp = now - 91.0;
    assert!(!geo.usable_geoclue(now - 120.0, now));
    geo.source = Source::Ip;
    assert!(geo.usable_geoclue(now - 120.0, now));
    let mut schedule = Schedule::default();
    schedule.observe(Some(network("a")), Instant::now());
    let since = schedule.network_since;
    schedule.retry(Instant::now(), Duration::ZERO);
    schedule.observe(Some(network("a")), Instant::now());
    schedule.rearm();
    assert_eq!(schedule.network_since, since);
    schedule.observe(None, Instant::now());
    assert!(schedule.network_since.is_none());
}

#[tokio::test]
async fn ip_fallback_skips_independent_lookup_even_during_its_cooldown() {
    let backend = Fake::default();
    backend.geo.lock().unwrap().source = Source::Ip;
    backend.geo.lock().unwrap().accuracy = 300_000.0;
    backend.pending_ip.store(true, Ordering::SeqCst);
    let (_dir, mut a) = authority(&backend).await;
    a.state.ip_not_before = wall_time() + 7200.0;
    check(&mut a, &backend).await;
    assert_eq!(backend.ip_calls.load(Ordering::SeqCst), 0);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    let accepted = a.storage.load().unwrap().accepted.unwrap();
    assert_eq!(accepted.source, Source::Ip);
    assert_eq!(accepted.accuracy_meters, Some(300_000.0));
    assert_eq!(accepted.timezone, "Europe/Dublin");
}

#[tokio::test]
async fn non_ip_and_unknown_require_confirmation_and_same_zone_updates_location_without_write() {
    for source in [Source::NonIp, Source::Unknown] {
        let backend = Fake::default();
        backend.geo.lock().unwrap().source = source;
        *backend.zone.lock().unwrap() = "Eire".into();
        let (_dir, mut a) = authority(&backend).await;
        check(&mut a, &backend).await;
        assert_eq!(backend.ip_calls.load(Ordering::SeqCst), 1);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        assert_eq!(a.state.accepted.as_ref().unwrap().source, source);
        assert_eq!(a.state.accepted.as_ref().unwrap().timezone, "Europe/Dublin");
        assert!(
            !a.schedule
                .ready(Instant::now() + Duration::from_secs(86400))
        );
    }
}

#[tokio::test]
async fn failed_invalid_or_interrupted_geoclue_does_not_spend_ip_request() {
    for scenario in ["failure", "invalid", "network"] {
        let backend = Fake::default();
        match scenario {
            "failure" => backend.geo_failed.store(true, Ordering::SeqCst),
            "invalid" => backend.geo.lock().unwrap().accuracy = -1.0,
            _ => backend.geo_changes_network.store(true, Ordering::SeqCst),
        }
        let (_dir, mut a) = authority(&backend).await;
        check(&mut a, &backend).await;
        assert_eq!(backend.ip_calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        assert!(a.schedule.ready(Instant::now() + Duration::from_secs(31)));
    }
    let backend = Fake::default();
    backend.geo.lock().unwrap().source = Source::Ip;
    backend.geo_changes_network.store(true, Ordering::SeqCst);
    let (_dir, mut a) = authority(&backend).await;
    check(&mut a, &backend).await;
    assert_eq!(
        backend.calls.load(Ordering::SeqCst),
        0,
        "IP exemption does not bypass network fence"
    );
}

#[tokio::test(start_paused = true)]
async fn both_acquisition_stages_are_bounded_and_cancel_on_network_sleep_reload_shutdown() {
    for ip_stage in [false, true] {
        for scenario in [
            "timeout",
            "network",
            "sleep",
            "reload",
            "shutdown",
            "sleep_cycle",
        ] {
            let backend = Fake::default();
            if ip_stage {
                backend.pending_ip.store(true, Ordering::SeqCst);
            } else {
                backend.pending_geo.store(true, Ordering::SeqCst);
            }
            let (_dir, mut a) = authority(&backend).await;
            let (sender, mut control) = watch::channel(Control::default());
            let interrupt = async {
                if scenario == "timeout" {
                    return;
                }
                let live = if ip_stage {
                    &backend.ip_live
                } else {
                    &backend.geo_live
                };
                while !live.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
                if scenario == "network" {
                    *backend.network.lock().unwrap() = network("b");
                } else {
                    sender.send_modify(|c| {
                        c.epoch += 1;
                        c.sleeping = scenario == "sleep";
                        c.shutdown = scenario == "shutdown";
                        if scenario == "reload" {
                            c.reload += 1;
                        }
                    });
                }
            };
            let (result, ()) = tokio::join!(a.check(&backend, &mut control), interrupt);
            result.unwrap();
            assert!(!backend.geo_live.load(Ordering::SeqCst));
            assert!(!backend.ip_live.load(Ordering::SeqCst));
            assert_eq!(backend.calls.load(Ordering::SeqCst), 0, "{scenario}");
            assert_eq!(
                backend.ip_calls.load(Ordering::SeqCst),
                usize::from(ip_stage)
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn ip_failure_retries_and_persisted_cooldown_does_not_block_later_geoclue_ip() {
    let backend = Fake::default();
    backend.ip_unavailable.store(true, Ordering::SeqCst);
    let (dir, mut a) = authority(&backend).await;
    check(&mut a, &backend).await;
    assert!(a.schedule.ready(Instant::now() + Duration::from_secs(30)));
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    backend.limited.store(true, Ordering::SeqCst);
    check(&mut a, &backend).await;
    assert!(a.storage.load().unwrap().ip_not_before >= wall_time() + 7199.0);
    let mut a = Authority::new(
        Storage::new(dir.path().join("state"), dir.path().join("run")),
        zones(),
        &backend,
    )
    .await
    .unwrap();
    a.schedule.observe(Some(network("a")), Instant::now());
    check(&mut a, &backend).await;
    assert_eq!(
        backend.ip_calls.load(Ordering::SeqCst),
        2,
        "restart cannot bypass 429"
    );
    backend.geo.lock().unwrap().source = Source::Ip;
    let (sender, control) = watch::channel(Control {
        reload: 1,
        epoch: 1,
        ..Control::default()
    });
    let stop = async {
        tokio::time::sleep(Duration::from_secs(40)).await;
        sender.send_modify(|c| c.shutdown = true);
    };
    let (result, ()) = tokio::join!(a.run(&backend, control), stop);
    result.unwrap();
    assert_eq!(backend.ip_calls.load(Ordering::SeqCst), 2);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert!(a.storage.load().unwrap().ip_not_before > wall_time());
}

#[tokio::test]
async fn ready_reply_racing_shutdown_persists_429_but_never_applies_a_fix() {
    for limited in [false, true] {
        let backend = Fake::default();
        backend.limited.store(limited, Ordering::SeqCst);
        let (_dir, mut a) = authority(&backend).await;
        let (sender, mut control) = watch::channel(Control::default());
        *backend.cancel_on_ip.lock().unwrap() = Some(sender);
        a.check(&backend, &mut control).await.unwrap();
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            a.storage.load().unwrap().ip_not_before > wall_time(),
            limited
        );
        assert!(a.state.accepted.is_none());
        assert!(!a.state.write_pending);
    }
}

#[tokio::test]
async fn legacy_provider_cooldown_is_preserved_but_not_repopulated() {
    let backend = Fake::default();
    let (dir, mut a) = authority(&backend).await;
    a.state.wifi_not_before = wall_time() + 7200.0;
    a.storage.save(&a.state).await.unwrap();
    let mut a = Authority::new(
        Storage::new(dir.path().join("state"), dir.path().join("run")),
        zones(),
        &backend,
    )
    .await
    .unwrap();
    a.schedule.observe(Some(network("a")), Instant::now());
    check(&mut a, &backend).await;
    assert_eq!(backend.geo_calls.load(Ordering::SeqCst), 0);
    assert!(a.state.geoclue_cooldown(wall_time()).as_secs() >= 7198);
}

#[test]
fn retries_preserve_backoff_across_flicker_but_completed_checks_stay_idle() {
    let mut s = Schedule::default();
    let mut now = Instant::now();
    s.observe(Some(network("a")), now);
    assert!(!s.ready(now + Duration::from_secs(9)));
    now += Duration::from_secs(10);
    for delay in [30, 60, 120, 240, 300, 300] {
        assert!(s.ready(now));
        s.retry(now, Duration::ZERO);
        s.observe(None, now);
        assert!(!s.ready(now + Duration::from_secs(10000)));
        s.observe(Some(network("a")), now + Duration::from_secs(1));
        now += Duration::from_secs(delay);
        assert!(!s.ready(now - Duration::from_secs(1)));
    }
    s.retry(now, Duration::from_secs(7200));
    assert!(!s.ready(now + Duration::from_secs(7199)));
    assert!(s.ready(now + Duration::from_secs(7200)));
    s.complete();
    s.observe(None, now);
    s.observe(Some(network("a")), now);
    assert!(!s.ready(now + Duration::from_secs(86400)));
    s.observe(Some(network("b")), now);
    assert!(s.ready(now + Duration::from_secs(10)));
}

#[tokio::test(start_paused = true)]
async fn disagreement_manual_override_and_uncertain_write_keep_previous_accepted_state() {
    for scenario in ["disagreement", "manual", "lost_reply", "wrong_readback"] {
        let backend = Fake::default();
        let (dir, mut a) = authority(&backend).await;
        check(&mut a, &backend).await;
        let previous = a.state.accepted.as_ref().unwrap().verified_at;
        *backend.zone.lock().unwrap() = "America/Edmonton".into();
        if scenario != "manual" {
            a.baseline = "America/Edmonton".into();
        }
        match scenario {
            "disagreement" => {
                backend.geo.lock().unwrap().latitude = 54.6056;
                backend.geo.lock().unwrap().longitude = -112.803852;
            }
            "lost_reply" => backend.lost_reply.store(true, Ordering::SeqCst),
            "wrong_readback" => backend.wrong_readback.store(true, Ordering::SeqCst),
            _ => {}
        }
        check(&mut a, &backend).await;
        assert_eq!(a.state.accepted.as_ref().unwrap().verified_at, previous);
        let uncertain = matches!(scenario, "lost_reply" | "wrong_readback");
        assert_eq!(a.state.write_pending, uncertain);
        assert_eq!(a.state.paused, scenario != "disagreement");
        assert_eq!(
            backend.calls.load(Ordering::SeqCst),
            1 + usize::from(uncertain)
        );
        assert!(
            !a.schedule
                .ready(Instant::now() + Duration::from_secs(86400))
        );
        if uncertain {
            let mut restarted = Authority::new(
                Storage::new(dir.path().join("state"), dir.path().join("run")),
                zones(),
                &backend,
            )
            .await
            .unwrap();
            let before = backend.geo_calls.load(Ordering::SeqCst);
            let (sender, control) = watch::channel(Control::default());
            let stop = async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                sender.send_modify(|c| c.shutdown = true);
            };
            let (result, ()) = tokio::join!(restarted.run(&backend, control), stop);
            result.unwrap();
            assert_eq!(backend.geo_calls.load(Ordering::SeqCst), before);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn startup_detects_offline_override_and_reload_acknowledgement_survives_restart() {
    for legacy in [false, true] {
        let backend = Fake::default();
        let (dir, mut a) = authority(&backend).await;
        check(&mut a, &backend).await;
        if legacy {
            a.state.expected_timezone = None;
            a.state.accepted.as_mut().unwrap().timezone = "Eire".into();
            a.storage.save(&a.state).await.unwrap();
        }
        let storage = || Storage::new(dir.path().join("state"), dir.path().join("run"));
        *backend.zone.lock().unwrap() = "Eire".into();
        assert!(
            !Authority::new(storage(), zones(), &backend)
                .await
                .unwrap()
                .state
                .paused
        );
        *backend.zone.lock().unwrap() = "America/Edmonton".into();
        let mut restarted = Authority::new(storage(), zones(), &backend).await.unwrap();
        assert!(restarted.storage.load().unwrap().paused);
        let before = backend.geo_calls.load(Ordering::SeqCst);
        let (sender, control) = watch::channel(Control::default());
        let stop = async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            sender.send_modify(|c| c.shutdown = true);
        };
        let (result, ()) = tokio::join!(restarted.run(&backend, control), stop);
        result.unwrap();
        assert_eq!(backend.geo_calls.load(Ordering::SeqCst), before);
        let (sender, control) = watch::channel(Control {
            reload: 1,
            ..Control::default()
        });
        let stop = async {
            // Stop before the debounced lookup: acknowledgement alone must persist.
            tokio::time::sleep(Duration::from_secs(1)).await;
            sender.send_modify(|c| c.shutdown = true);
        };
        let (result, ()) = tokio::join!(restarted.run(&backend, control), stop);
        result.unwrap();
        let restarted = Authority::new(storage(), zones(), &backend).await.unwrap();
        assert!(!restarted.state.paused);
        assert_eq!(
            restarted.state.expected_timezone.as_deref(),
            Some("America/Edmonton")
        );
        assert_eq!(backend.geo_calls.load(Ordering::SeqCst), before);
    }
}

#[tokio::test]
async fn legacy_state_migrates_and_failed_persistence_blocks_timezone_effects() {
    let backend = Fake::default();
    let (dir, mut a) = authority(&backend).await;
    std::fs::write(&a.storage.state_path, r#"{"ip_calls":40,"budget_day":"old","ip_not_before":9999999999,"paused":true,"accepted":{"timezone":"Europe/Dublin","latitude":53.3,"longitude":-6.2,"verified_at":1000}}"#).unwrap();
    let state = a.storage.load().unwrap();
    a.storage.save(&state).await.unwrap();
    assert!(state.paused);
    assert_eq!(state.accepted.as_ref().unwrap().source, Source::Unknown);
    assert_eq!(state.accepted.as_ref().unwrap().accuracy_meters, None);
    assert!(
        !std::fs::read_to_string(&a.storage.state_path)
            .unwrap()
            .contains("ip_calls")
    );
    std::fs::remove_file(&a.storage.state_path).unwrap();
    std::fs::create_dir(&a.storage.state_path).unwrap();
    let (_sender, mut control) = watch::channel(Control::default());
    assert!(a.check(&backend, &mut control).await.is_err());
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    std::fs::remove_dir(&a.storage.state_path).unwrap();
    std::os::unix::fs::symlink(dir.path().join("other"), &a.storage.state_path).unwrap();
    assert!(a.storage.load().is_err());
}

#[tokio::test]
async fn durable_files_retain_private_coordinates_and_public_status_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let backend = Fake::default();
    let (_dir, mut a) = authority(&backend).await;
    check(&mut a, &backend).await;
    assert_eq!(
        std::fs::metadata(&a.storage.state_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&a.storage.status_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    assert_eq!(
        std::fs::read_to_string(&a.storage.status_path).unwrap(),
        r#"{"message":""}"#
    );
}
