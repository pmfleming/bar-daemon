//! Actual GeoClue/NM/timedated serialization on a private bus; no host services.
use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};
use tokio::sync::Notify;
use zbus::{Connection, object_server::SignalEmitter};
use zvariant::OwnedObjectPath;

#[derive(Default)]
struct Services {
    fresh: AtomicBool,
    malformed_time: AtomicBool,
    deny_start: AtomicBool,
    source: AtomicU32,
    age: AtomicU64,
    refresh_supported: AtomicBool,
    refreshes: AtomicUsize,
    desktop: Mutex<String>,
    accuracy: AtomicU32,
    started: Notify,
    deleted: Notify,
    writes: AtomicUsize,
    ip_calls: AtomicUsize,
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
    fn start(&self) -> zbus::fdo::Result<()> {
        assert_eq!(*self.0.desktop.lock().unwrap(), "location-timezone");
        assert_eq!(self.0.accuracy.load(Ordering::SeqCst), 4);
        self.0.started.notify_one();
        if self.0.deny_start.load(Ordering::SeqCst) {
            Err(zbus::fdo::Error::AccessDenied("denied".into()))
        } else {
            Ok(())
        }
    }
    async fn refresh_since(
        &self,
        minimum: u64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        if !self.0.refresh_supported.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::UnknownMethod("unpatched GeoClue".into()));
        }
        assert!(minimum <= wall_time() as u64);
        assert_eq!(self.0.refreshes.fetch_add(1, Ordering::SeqCst), 0);
        self.0.age.store(0, Ordering::SeqCst);
        self.publish(emitter).await
    }
    async fn publish(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.0.fresh.store(true, Ordering::SeqCst);
        self.0.malformed_time.store(false, Ordering::SeqCst);
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
        300_000.0
    }
    #[zbus(property)]
    fn description(&self) -> &str {
        "GeoIP"
    } // deliberately misleading for non-IP/unknown
    #[zbus(property)]
    fn source_kind(&self) -> u32 {
        self.0.source.load(Ordering::SeqCst)
    }
    #[zbus(property)]
    fn timestamp(&self) -> (u64, u64) {
        let seconds = wall_time() as u64;
        (
            if self.0.fresh.load(Ordering::SeqCst) {
                seconds - self.0.age.load(Ordering::SeqCst)
            } else {
                seconds - 600
            },
            if self.0.malformed_time.load(Ordering::SeqCst) {
                1_000_000
            } else {
                0
            },
        )
    }
}
struct LegacyLocation;
#[zbus::interface(name = "org.freedesktop.GeoClue2.Location")]
impl LegacyLocation {
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
        300_000.0
    }
    #[zbus(property)]
    fn description(&self) -> &str {
        "ipf fallback (from WiFi data)"
    }
    #[zbus(property)]
    fn timestamp(&self) -> (u64, u64) {
        (wall_time() as u64, 0)
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
    async fn geoclue(&self, started: f64) -> Result<Fix> {
        geoclue::locate_on(&self.bus.connection, started).await
    }
    async fn ip(&self) -> http::LookupResult {
        self.state.ip_calls.fetch_add(1, Ordering::SeqCst);
        http::LookupResult::Fix(tests::fix(Source::Ip))
    }
}
async fn peer(legacy: bool) -> (tokio::process::Child, Connection, Peer, Arc<Services>) {
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
        .unwrap();
    let server = if legacy {
        server
            .serve_at("/fixture/location", LegacyLocation)
            .unwrap()
    } else {
        server
            .serve_at("/fixture/location", Location(state.clone()))
            .unwrap()
    };
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

#[tokio::test]
async fn real_bus_provenance_controls_ip_confirmation_not_description_or_accuracy() {
    for (legacy, source) in [(false, 1), (false, 2), (false, 0), (false, 999), (true, 1)] {
        let (_daemon, _server, peer, state) = peer(legacy).await;
        state.fresh.store(true, Ordering::SeqCst);
        state.source.store(source, Ordering::SeqCst);
        let (_dir, mut a) = tests::authority(&peer).await;
        a.schedule
            .observe(peer.observe().await.unwrap().network, Instant::now());
        let (_sender, mut control) = watch::channel(Control::default());
        tokio::time::timeout(Duration::from_secs(5), async {
            a.check(&peer, &mut control).await.unwrap();
            assert!(a.state.accepted.is_some(), "{}", a.last_result);
            state.deleted.notified().await;
        })
        .await
        .unwrap();
        assert_eq!(state.writes.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.ip_calls.load(Ordering::SeqCst),
            usize::from(legacy || source != 1)
        );
        assert_eq!(
            a.state.accepted.as_ref().unwrap().source,
            if legacy {
                Source::Unknown
            } else {
                Source::from_geoclue(source)
            }
        );
    }
}

#[tokio::test]
async fn stale_or_malformed_fix_waits_for_update_then_cleans_up() {
    for malformed in [false, true] {
        let (_daemon, _server, peer, state) = peer(false).await;
        state.source.store(1, Ordering::SeqCst);
        state.fresh.store(malformed, Ordering::SeqCst);
        state.malformed_time.store(malformed, Ordering::SeqCst);
        let publish = async {
            state.started.notified().await;
            tokio::time::sleep(Duration::from_millis(40)).await;
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
            let (result, ()) = tokio::join!(peer.geoclue(wall_time()), publish);
            assert_eq!(result.unwrap().source, Source::Ip);
            state.deleted.notified().await;
        })
        .await
        .unwrap();
        assert_eq!(state.writes.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn shared_source_recent_cache_is_reused_and_stale_cache_is_revalidated_once() {
    for source in [0, 1, 2] {
        for age in [30, 100, 600] {
            let (_daemon, _server, peer, state) = peer(false).await;
            state.fresh.store(true, Ordering::SeqCst);
            state.source.store(source, Ordering::SeqCst);
            state.age.store(age, Ordering::SeqCst);
            state.refresh_supported.store(true, Ordering::SeqCst);
            let network_since = wall_time() - 60.0;
            let fix = tokio::time::timeout(Duration::from_secs(5), peer.geoclue(network_since))
                .await
                .unwrap()
                .unwrap();
            assert!(fix.usable_geoclue(network_since, wall_time()));
            assert_eq!(
                state.refreshes.load(Ordering::SeqCst),
                usize::from(age > 60)
            );
        }
    }
}

#[tokio::test]
async fn cancellation_and_denial_release_geoclue_client_without_effects() {
    for denied in [false, true] {
        let (_daemon, _server, peer, state) = peer(false).await;
        state.deny_start.store(denied, Ordering::SeqCst);
        if denied {
            assert!(peer.geoclue(wall_time()).await.is_err());
        } else {
            let mut pending = Box::pin(peer.geoclue(wall_time()));
            tokio::select! {
                result = &mut pending => panic!("stale fix must not complete: {result:?}"),
                _ = state.started.notified() => {},
            }
            drop(pending);
        }
        tokio::time::timeout(Duration::from_secs(2), state.deleted.notified())
            .await
            .unwrap();
        assert_eq!(state.writes.load(Ordering::SeqCst), 0);
        assert_eq!(state.ip_calls.load(Ordering::SeqCst), 0);
    }
}
