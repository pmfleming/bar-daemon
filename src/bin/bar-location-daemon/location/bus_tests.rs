//! Actual D-Bus serialization on a private message bus; no host services or HTTP.
use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
};
use tokio::sync::Notify;
use zbus::{Connection, object_server::SignalEmitter};
use zvariant::OwnedObjectPath;

#[derive(Default)]
struct Services {
    fresh: AtomicBool,
    desktop: Mutex<String>,
    accuracy: AtomicU32,
    started: Notify,
    deleted: Notify,
    writes: AtomicUsize,
    zone: Mutex<String>,
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
struct Manager(Arc<Services>);
#[zbus::interface(name = "org.freedesktop.GeoClue2.Manager")]
impl Manager {
    fn get_client(&self) -> OwnedObjectPath {
        "/fixture/client".try_into().unwrap()
    }
    fn delete_client(&self, path: OwnedObjectPath) {
        assert_eq!(path.as_str(), "/fixture/client");
        self.0.deleted.notify_one();
    }
}
struct Client(Arc<Services>);
#[zbus::interface(name = "org.freedesktop.GeoClue2.Client")]
impl Client {
    #[zbus(property)]
    fn desktop_id(&self) -> String {
        self.0.desktop.lock().unwrap().clone()
    }
    #[zbus(property)]
    fn set_desktop_id(&self, value: &str) {
        *self.0.desktop.lock().unwrap() = value.into();
    }
    #[zbus(property)]
    fn requested_accuracy_level(&self) -> u32 {
        self.0.accuracy.load(Ordering::SeqCst)
    }
    #[zbus(property)]
    fn set_requested_accuracy_level(&self, value: u32) {
        self.0.accuracy.store(value, Ordering::SeqCst);
    }
    #[zbus(property)]
    fn location(&self) -> OwnedObjectPath {
        "/fixture/location".try_into().unwrap()
    }
    fn start(&self) {
        assert_eq!(*self.0.desktop.lock().unwrap(), "location-timezone");
        assert_eq!(self.0.accuracy.load(Ordering::SeqCst), 4);
        self.0.started.notify_one();
    }
    async fn publish(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.0.fresh.store(true, Ordering::SeqCst);
        Self::location_updated(
            &emitter,
            "/fixture/location".try_into().unwrap(),
            "/fixture/location".try_into().unwrap(),
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
    #[zbus(signal)]
    async fn location_updated(
        emitter: &SignalEmitter<'_>,
        old: OwnedObjectPath,
        new: OwnedObjectPath,
    ) -> zbus::Result<()>;
}
struct Location(Arc<Services>);
#[zbus::interface(name = "org.freedesktop.GeoClue2.Location")]
impl Location {
    #[zbus(property)]
    fn latitude(&self) -> f64 {
        53.3498
    }
    #[zbus(property)]
    fn longitude(&self) -> f64 {
        -6.2603
    }
    #[zbus(property)]
    fn accuracy(&self) -> f64 {
        if self.0.fresh.load(Ordering::SeqCst) {
            1000.0
        } else {
            26000.0
        }
    }
    #[zbus(property)]
    fn description(&self) -> &str {
        if self.0.fresh.load(Ordering::SeqCst) {
            "WiFi"
        } else {
            "ipf fallback (from WiFi data)"
        }
    }
    #[zbus(property)]
    fn timestamp(&self) -> (u64, u64) {
        let ms = chrono::Utc::now().timestamp_millis() as u64;
        (ms / 1000, (ms % 1000) * 1000)
    }
}
struct Peer {
    bus: bus::Bus,
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
    async fn wifi(&self, started: f64) -> Result<Fix> {
        bus::wifi_on(&self.bus.connection, started).await
    }
    async fn ip(&self) -> http::IpResult {
        http::IpResult::Fix(tests::fix("IP"))
    }
}
async fn peer() -> (tokio::process::Child, Connection, Peer, Arc<Services>) {
    let state = Arc::new(Services::default());
    *state.zone.lock().unwrap() = "America/Edmonton".into();
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
    let server = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .name("org.freedesktop.NetworkManager")
        .unwrap()
        .name("org.freedesktop.GeoClue2")
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
        .serve_at("/org/freedesktop/GeoClue2/Manager", Manager(state.clone()))
        .unwrap()
        .serve_at("/fixture/client", Client(state.clone()))
        .unwrap()
        .serve_at("/fixture/location", Location(state.clone()))
        .unwrap()
        .build();
    let client = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .method_timeout(Duration::from_secs(1))
        .build();
    let (server, connection) = tokio::try_join!(server, client).unwrap();
    (
        daemon,
        server,
        Peer {
            bus: bus::Bus {
                connection,
                http: http::client().unwrap(),
            },
        },
        state,
    )
}

#[tokio::test]
async fn real_bus_waits_through_fallback_then_verifies_one_write_and_cleans_up() {
    let (_daemon, _server, peer, state) = peer().await;
    let (_dir, mut a) = tests::authority(&peer).await;
    a.schedule
        .observe(peer.observe().await.unwrap().network, Instant::now());
    let (_sender, mut control) = watch::channel(Control::default());
    let publish = async {
        state.started.notified().await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(state.writes.load(Ordering::SeqCst), 0);
        let client = zbus::Proxy::new(
            &peer.bus.connection,
            "org.freedesktop.GeoClue2",
            "/fixture/client",
            "org.freedesktop.GeoClue2.Client",
        )
        .await
        .unwrap();
        client.call::<_, _, ()>("Publish", &()).await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        let (result, ()) = tokio::join!(a.check(&peer, &mut control), publish);
        result.unwrap();
        state.deleted.notified().await;
    })
    .await
    .unwrap();
    assert_eq!(state.writes.load(Ordering::SeqCst), 1);
    assert_eq!(
        a.storage.load().unwrap().accepted.unwrap().timezone,
        "Europe/Dublin"
    );
}

#[tokio::test]
async fn cancelling_real_geoclue_request_releases_its_client() {
    let (_daemon, _server, peer, state) = peer().await;
    let mut pending = Box::pin(peer.wifi(wall_time()));
    tokio::select! {
        _ = &mut pending => panic!("fallback must not be accepted"),
        _ = state.started.notified() => {},
    }
    drop(pending);
    tokio::time::timeout(Duration::from_secs(2), state.deleted.notified())
        .await
        .unwrap();
    assert_eq!(state.writes.load(Ordering::SeqCst), 0);
}
