//! Real D-Bus serialization on a private bus; never touches host services.
use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
};
use tokio::sync::Notify;
use zbus::Connection;
use zvariant::{OwnedObjectPath, OwnedValue};

#[derive(Default)]
struct Services {
    last_scan: AtomicI64,
    requested: Notify,
    writes: AtomicUsize,
    completed_scans: AtomicUsize,
    zone: Mutex<String>,
    usable_aps: AtomicBool,
    deny_scan: AtomicBool,
}
struct NetworkManager;
#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl NetworkManager {
    #[zbus(property)]
    fn connectivity(&self) -> u32 {
        4
    }
    #[zbus(property)]
    fn state(&self) -> u32 {
        70
    }
    #[zbus(property)]
    fn active_connections(&self) -> Vec<OwnedObjectPath> {
        vec!["/connection/a".try_into().unwrap()]
    }
    #[zbus(property)]
    fn primary_connection(&self) -> OwnedObjectPath {
        "/connection/a".try_into().unwrap()
    }
    fn get_devices(&self) -> Vec<OwnedObjectPath> {
        vec!["/fixture/wifi".try_into().unwrap()]
    }
}
struct Device;
#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl Device {
    #[zbus(property)]
    fn device_type(&self) -> u32 {
        2
    }
    #[zbus(property)]
    fn managed(&self) -> bool {
        true
    }
}
struct Wireless(Arc<Services>);
#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl Wireless {
    #[zbus(property)]
    fn last_scan(&self) -> i64 {
        self.0.last_scan.load(Ordering::SeqCst)
    }
    fn request_scan(
        &self,
        options: std::collections::HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        assert!(options.is_empty());
        self.0.requested.notify_one();
        if self.0.deny_scan.load(Ordering::SeqCst) {
            Err(zbus::fdo::Error::AccessDenied("scan denied".into()))
        } else {
            Ok(())
        }
    }
    fn get_all_access_points(&self) -> Vec<OwnedObjectPath> {
        (0..7)
            .map(|i| format!("/fixture/ap{i}").try_into().unwrap())
            .collect()
    }
}
struct AccessPoint(Arc<Services>, usize);
#[zbus::interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl AccessPoint {
    #[zbus(property)]
    fn hw_address(&self) -> &str {
        match self.1 {
            0 => "00:11:22:33:44:55",
            1 => "00:11:22:33:44:66",
            2 => "7a:11:22:29:92:b4",
            3 => "00:11:22:33:44:77",
            4 => "00:11:22:33:44:88",
            5 => "00:11:22:33:44:99",
            _ => "00:11:22:33:44:55", // duplicate is not independent evidence
        }
    }
    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        if self.1 == 3 {
            b"private_nomap".to_vec()
        } else {
            b"fixture".to_vec()
        }
    }
    #[zbus(property)]
    fn strength(&self) -> u8 {
        if self.1 == 5 { 1 } else { 80 }
    }
    #[zbus(property)]
    fn last_seen(&self) -> i32 {
        if self.1 == 4 || (self.1 == 1 && !self.0.usable_aps.load(Ordering::SeqCst)) {
            return 0;
        }
        (self.0.last_scan.load(Ordering::SeqCst) / 1000) as i32
    }
}
struct Login;
#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl Login {
    #[zbus(property)]
    fn preparing_for_sleep(&self) -> bool {
        false
    }
}
struct Timedated(Arc<Services>);
#[zbus::interface(name = "org.freedesktop.timedate1")]
impl Timedated {
    #[zbus(property)]
    fn timezone(&self) -> String {
        self.0.zone.lock().unwrap().clone()
    }
    fn set_timezone(&self, zone: &str, interactive: bool) {
        assert!(!interactive);
        assert_eq!(zone, "Europe/Dublin");
        self.0.writes.fetch_add(1, Ordering::SeqCst);
        *self.0.zone.lock().unwrap() = zone.into();
    }
}
struct Peer {
    bus: bus::Bus,
    state: Arc<Services>,
}
impl Backend for Peer {
    async fn observe(&self) -> Result<Observation> {
        self.bus.observe().await
    }
    async fn timezone(&self) -> Result<String> {
        self.bus.timezone().await
    }
    async fn set_timezone(&self, zone: &str) -> Result<()> {
        self.bus.set_timezone(zone).await
    }
    async fn wifi(&self, _: f64) -> Result<http::LookupResult> {
        let _scan = wifi::scan(&self.bus.connection).await?;
        self.state.completed_scans.fetch_add(1, Ordering::SeqCst);
        // HTTP serialization/response policy are independently exercised by wifi::tests.
        Ok(http::LookupResult::Fix(tests::fix("WiFi")))
    }
    async fn ip(&self) -> http::LookupResult {
        http::LookupResult::Fix(tests::fix("IP"))
    }
}

async fn peer() -> (tokio::process::Child, Connection, Peer, Arc<Services>) {
    let state = Arc::new(Services::default());
    *state.zone.lock().unwrap() = "America/Edmonton".into();
    state.usable_aps.store(true, Ordering::SeqCst);
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
    let mut lines = tokio::io::BufReader::new(daemon.stdout.take().unwrap()).lines();
    let address = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mut server = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .name("org.freedesktop.NetworkManager")
        .unwrap()
        .name("org.freedesktop.login1")
        .unwrap()
        .name("org.freedesktop.timedate1")
        .unwrap()
        .serve_at("/org/freedesktop/NetworkManager", NetworkManager)
        .unwrap()
        .serve_at("/org/freedesktop/login1", Login)
        .unwrap()
        .serve_at("/org/freedesktop/timedate1", Timedated(state.clone()))
        .unwrap()
        .serve_at("/fixture/wifi", Device)
        .unwrap()
        .serve_at("/fixture/wifi", Wireless(state.clone()))
        .unwrap();
    for i in 0..7 {
        server = server
            .serve_at(format!("/fixture/ap{i}"), AccessPoint(state.clone(), i))
            .unwrap();
    }
    let client = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .method_timeout(Duration::from_secs(1))
        .build();
    let (server, connection) = tokio::try_join!(server.build(), client).unwrap();
    (
        daemon,
        server,
        Peer {
            bus: bus::Bus {
                connection,
                http: http::client().unwrap(),
            },
            state: state.clone(),
        },
        state,
    )
}

async fn publish_scan(state: &Services) {
    state.requested.notified().await;
    // LastSeen is whole seconds: publish strictly after the request's second.
    let now = wifi::boottime_ms().unwrap();
    tokio::time::sleep(Duration::from_millis((1100 - now % 1000) as u64)).await;
    state
        .last_scan
        .store(wifi::boottime_ms().unwrap(), Ordering::SeqCst);
}

#[tokio::test]
async fn real_bus_waits_for_new_scan_then_verifies_one_write() {
    let (_daemon, _server, peer, state) = peer().await;
    let (_dir, mut authority) = tests::authority(&peer).await;
    authority
        .schedule
        .observe(peer.observe().await.unwrap().network, Instant::now());
    let (_sender, mut control) = watch::channel(Control::default());
    let publish = async {
        publish_scan(&state).await;
        assert_eq!(state.writes.load(Ordering::SeqCst), 0);
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        let (result, ()) = tokio::join!(authority.check(&peer, &mut control), publish);
        result.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(state.completed_scans.load(Ordering::SeqCst), 1);
    assert_eq!(state.writes.load(Ordering::SeqCst), 1);
    assert_eq!(
        authority.storage.load().unwrap().accepted.unwrap().timezone,
        "Europe/Dublin"
    );
}

#[tokio::test]
async fn filtered_stale_and_duplicate_aps_cannot_supply_missing_evidence() {
    let (_daemon, _server, peer, state) = peer().await;
    state.usable_aps.store(false, Ordering::SeqCst);
    let (result, ()) = tokio::join!(peer.wifi(wall_time()), publish_scan(&state));
    assert!(
        result.is_err(),
        "one good AP plus duplicates/unsuitable APs is insufficient"
    );
    assert_eq!(state.completed_scans.load(Ordering::SeqCst), 0);
    assert_eq!(state.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cached_scan_is_not_accepted_and_cancellation_cannot_finish_acquisition() {
    let (_daemon, _server, peer, state) = peer().await;
    state
        .last_scan
        .store(wifi::boottime_ms().unwrap() - 5000, Ordering::SeqCst);
    let mut pending = Box::pin(peer.wifi(wall_time()));
    tokio::select! {
        _ = &mut pending => panic!("cached scan must not complete"),
        _ = state.requested.notified() => {},
    }
    tokio::select! {
        _ = &mut pending => panic!("must wait for LastScan to advance"),
        _ = tokio::time::sleep(Duration::from_millis(50)) => {},
    }
    drop(pending);
    state
        .last_scan
        .store(wifi::boottime_ms().unwrap(), Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(state.completed_scans.load(Ordering::SeqCst), 0);
    assert_eq!(state.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn scan_denied_never_falls_back_to_cached_aps() {
    let (_daemon, _server, peer, state) = peer().await;
    state.deny_scan.store(true, Ordering::SeqCst);
    assert!(peer.wifi(wall_time()).await.is_err());
    assert_eq!(state.completed_scans.load(Ordering::SeqCst), 0);
    assert_eq!(state.writes.load(Ordering::SeqCst), 0);
}
