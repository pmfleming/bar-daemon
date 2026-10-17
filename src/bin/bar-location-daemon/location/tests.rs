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
pub(super) fn fix(source: &str) -> Fix {
    Fix {
        latitude: 53.3498,
        longitude: -6.2603,
        timestamp: wall_time(),
        source: source.into(),
        accuracy: 1000.0,
        timezone: "Europe/Dublin".into(),
        country_code: "IE".into(),
    }
}
fn zones() -> Zones {
    Zones::from_tables(
        "L Europe/Dublin Eire\n",
        "IE +5320-00615 Europe/Dublin\nCA +5333-11328 America/Edmonton\n",
    )
    .unwrap()
}
struct Fake {
    network: Mutex<Network>,
    zone: Mutex<String>,
    pending_wifi: AtomicBool,
    wifi_live: AtomicBool,
    calls: AtomicUsize,
    lost_reply: AtomicBool,
    wrong_wifi: AtomicBool,
    limited: AtomicBool,
    wifi_limited: AtomicBool,
    ip_calls: AtomicUsize,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            network: Mutex::new(network("a")),
            zone: Mutex::new("America/Edmonton".into()),
            pending_wifi: AtomicBool::new(false),
            wifi_live: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            lost_reply: AtomicBool::new(false),
            wrong_wifi: AtomicBool::new(false),
            limited: AtomicBool::new(false),
            wifi_limited: AtomicBool::new(false),
            ip_calls: AtomicUsize::new(0),
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
        *self.zone.lock().unwrap() = zone.into();
        anyhow::ensure!(
            !self.lost_reply.load(Ordering::SeqCst),
            "reply lost after applying"
        );
        Ok(())
    }
    async fn wifi(&self, _: f64) -> Result<http::LookupResult> {
        self.wifi_live.store(true, Ordering::SeqCst);
        let _live = Live(&self.wifi_live);
        if self.pending_wifi.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.wifi_limited.load(Ordering::SeqCst) {
            return Ok(http::LookupResult::Limited(Duration::from_secs(7200)));
        }
        let mut result = fix("WiFi");
        if self.wrong_wifi.load(Ordering::SeqCst) {
            result.latitude = 54.6056;
            result.longitude = -112.803852;
        }
        Ok(http::LookupResult::Fix(result))
    }
    async fn ip(&self) -> http::LookupResult {
        self.ip_calls.fetch_add(1, Ordering::SeqCst);
        if self.limited.load(Ordering::SeqCst) {
            http::LookupResult::Limited(Duration::from_secs(7200))
        } else {
            http::LookupResult::Fix(fix("IP"))
        }
    }
}
pub(super) async fn authority(backend: &impl Backend) -> (TempDir, Authority) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::new(dir.path().join("state"), dir.path().join("run"));
    let mut authority = Authority::new(storage, zones(), backend).await.unwrap();
    authority
        .schedule
        .observe(Some(network("a")), Instant::now());
    (dir, authority)
}

#[tokio::test(start_paused = true)]
async fn cooldown_and_uncertain_write_fences_survive_restart() {
    let backend = Fake::default();
    backend.limited.store(true, Ordering::SeqCst);
    let (dir, mut a) = authority(&backend).await;
    let (_sender, mut control) = watch::channel(Control::default());
    a.check(&backend, &mut control).await.unwrap();
    assert!(a.storage.load().unwrap().ip_not_before >= wall_time() + 7199.0);
    let mut restarted = Authority::new(
        Storage::new(dir.path().join("state"), dir.path().join("run")),
        zones(),
        &backend,
    )
    .await
    .unwrap();
    restarted
        .schedule
        .observe(Some(network("b")), Instant::now());
    restarted.check(&backend, &mut control).await.unwrap();
    assert_eq!(
        backend.ip_calls.load(Ordering::SeqCst),
        1,
        "restart/new connection cannot bypass 429"
    );
    restarted.state.ip_not_before = 0.0;
    restarted.state.write_pending = true;
    restarted.storage.save(&restarted.state).await.unwrap();
    let mut restarted = Authority::new(
        Storage::new(dir.path().join("state"), dir.path().join("run")),
        zones(),
        &backend,
    )
    .await
    .unwrap();
    let (sender, control) = watch::channel(Control::default());
    let stop = async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        sender.send_modify(|s| s.shutdown = true);
    };
    let (result, ()) = tokio::join!(restarted.run(&backend, control), stop);
    result.unwrap();
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        backend.ip_calls.load(Ordering::SeqCst),
        1,
        "unverified write requires explicit recovery"
    );
}

#[tokio::test(start_paused = true)]
async fn wifi_cooldown_survives_restart_and_reload() {
    let backend = Fake::default();
    backend.wifi_limited.store(true, Ordering::SeqCst);
    let (dir, mut authority) = authority(&backend).await;
    let (_sender, mut control) = watch::channel(Control::default());
    authority.check(&backend, &mut control).await.unwrap();
    assert!(authority.storage.load().unwrap().wifi_not_before >= wall_time() + 7199.0);
    let mut restarted = Authority::new(
        Storage::new(dir.path().join("state"), dir.path().join("run")),
        zones(),
        &backend,
    )
    .await
    .unwrap();
    restarted.state.paused = true;
    let (sender, receiver) = watch::channel(Control {
        reload: 1,
        epoch: 1,
        ..Control::default()
    });
    let stop = async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        sender.send_modify(|state| state.shutdown = true);
    };
    let (result, ()) = tokio::join!(restarted.run(&backend, receiver), stop);
    result.unwrap();
    assert_eq!(restarted.reload, 1);
    assert!(!restarted.state.paused);
    assert_eq!(backend.ip_calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert!(restarted.state.cooldown(wall_time()).as_secs() >= 7198);
}

#[test]
fn evidence_requires_fresh_independent_matching_locations() {
    let zones = zones();
    let wifi = fix("WiFi");
    let ip = fix("IP");
    let started = wall_time() - 1.0;
    assert_eq!(
        zones.agree(&wifi, &ip, started, wall_time()).unwrap(),
        "Europe/Dublin"
    );
    assert_eq!(zones.canonical("Eire").as_deref(), Some("Europe/Dublin"));
    for change in ["old", "future", "fallback", "broad", "nan", "coordinate"] {
        let mut invalid = wifi.clone();
        match change {
            "old" => invalid.timestamp = started - 1.0,
            "future" => invalid.timestamp += 100.0,
            "fallback" => invalid.source = "ipf fallback (from WiFi data)".into(),
            "broad" => invalid.accuracy = 26000.0,
            "nan" => invalid.accuracy = f64::NAN,
            _ => invalid.latitude = 91.0,
        }
        assert!(
            matches!(
                zones.agree(&invalid, &ip, started, wall_time()),
                Err(Decision::Retry(_))
            ),
            "{change}"
        );
    }
    let mut foreign = wifi.clone();
    foreign.latitude = 54.6056;
    foreign.longitude = -112.803852;
    assert!(matches!(
        zones.agree(&foreign, &ip, started, wall_time()),
        Err(Decision::Stop(_))
    ));
    let mut ip = ip;
    ip.country_code = "CA".into();
    assert!(matches!(
        zones.agree(&wifi, &ip, started, wall_time()),
        Err(Decision::Stop(_))
    ));
    ip.country_code = "IE".into();
    ip.timezone = "Europe/London".into();
    assert!(matches!(
        zones.agree(&wifi, &ip, started, wall_time()),
        Err(Decision::Stop(_))
    ));
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
async fn no_wifi_retries_on_same_connection_then_applies_once() {
    let backend = Fake::default();
    backend.pending_wifi.store(true, Ordering::SeqCst);
    let (_dir, mut a) = authority(&backend).await;
    let (_sender, mut control) = watch::channel(Control::default());
    a.check(&backend, &mut control).await.unwrap();
    assert!(!backend.wifi_live.load(Ordering::SeqCst));
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert!(!a.schedule.ready(Instant::now() + Duration::from_secs(29)));
    assert!(a.schedule.ready(Instant::now() + Duration::from_secs(30)));
    backend.pending_wifi.store(false, Ordering::SeqCst);
    a.check(&backend, &mut control).await.unwrap();
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        a.storage.load().unwrap().accepted.unwrap().timezone,
        "Europe/Dublin"
    );
    assert!(
        !a.schedule
            .ready(Instant::now() + Duration::from_secs(86400))
    );
    use std::os::unix::fs::PermissionsExt;
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
}

#[tokio::test(start_paused = true)]
async fn network_and_sleep_changes_cancel_pending_work_without_writing() {
    for sleep in [false, true] {
        let backend = Fake::default();
        backend.pending_wifi.store(true, Ordering::SeqCst);
        let (_dir, mut a) = authority(&backend).await;
        let (sender, mut control) = watch::channel(Control::default());
        let change = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if sleep {
                sender.send_modify(|s| {
                    s.epoch += 1;
                    s.sleeping = true;
                });
            } else {
                *backend.network.lock().unwrap() = network("b");
            }
        };
        let (result, ()) = tokio::join!(a.check(&backend, &mut control), change);
        result.unwrap();
        assert!(!backend.wifi_live.load(Ordering::SeqCst));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn disagreement_manual_override_and_lost_reply_never_blindly_retry() {
    for scenario in ["disagreement", "manual", "unknown"] {
        let backend = Fake::default();
        let (_dir, mut a) = authority(&backend).await;
        match scenario {
            "disagreement" => backend.wrong_wifi.store(true, Ordering::SeqCst),
            "manual" => *backend.zone.lock().unwrap() = "Europe/Amsterdam".into(),
            _ => backend.lost_reply.store(true, Ordering::SeqCst),
        }
        let (_sender, mut control) = watch::channel(Control::default());
        a.check(&backend, &mut control).await.unwrap();
        assert_eq!(
            backend.calls.load(Ordering::SeqCst),
            usize::from(scenario == "unknown")
        );
        assert!(
            !a.schedule
                .ready(Instant::now() + Duration::from_secs(86400))
        );
        let stored = a.storage.load().unwrap();
        assert!(stored.accepted.is_none());
        assert_eq!(stored.paused, scenario != "disagreement");
        assert_eq!(stored.write_pending, scenario == "unknown");
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
    assert!(
        !std::fs::read_to_string(&a.storage.state_path)
            .unwrap()
            .contains("ip_calls")
    );
    assert_eq!(state.accepted.unwrap().timezone, "Europe/Dublin");
    std::fs::remove_file(&a.storage.state_path).unwrap();
    std::fs::create_dir(&a.storage.state_path).unwrap();
    let (_sender, mut control) = watch::channel(Control::default());
    assert!(a.check(&backend, &mut control).await.is_err());
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    std::fs::remove_dir(&a.storage.state_path).unwrap();
    std::os::unix::fs::symlink(dir.path().join("other"), &a.storage.state_path).unwrap();
    assert!(a.storage.load().is_err());
}
