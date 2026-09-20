//! In-process logind substitute. Tests never connect to the real system bus.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use tokio::{net::UnixStream, sync::Notify, time::timeout};
use zbus::{Connection, connection::Builder};

use super::*;

#[tokio::test]
async fn authorization_and_inhibition_fail_before_locking() {
    for capability in [
        "challenge",
        "inhibited",
        "inhibitor-blocked",
        "challenge-inhibitor-blocked",
        "unknown",
    ] {
        let state = Arc::new(SessionState {
            capability: Some(capability),
            ..Default::default()
        });
        let (_server, client) = fake_logind(state.clone()).await;
        for action in ["suspend", "hibernate", "suspend-then-hibernate"] {
            assert!(
                perform_connected(&client, action, Duration::from_secs(1))
                    .await
                    .is_err()
            );
        }
        assert_eq!(state.lock_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn final_trigger_validation_cancels_after_setup_and_preflight_queries() {
    let state = Arc::new(SessionState::default());
    state.locked.store(true, Ordering::SeqCst);
    let (_server, client) = fake_logind(state.clone()).await;
    let result = perform_connected_with_validation(
        &client,
        "suspend",
        Duration::from_secs(1),
        false,
        || std::future::ready(Ok(())),
        || std::future::ready(Err(anyhow::anyhow!("idle episode ended"))),
        &outcome::Tracker::default(),
    )
    .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("idle episode ended")
    );
    assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn trigger_validation_cannot_hide_a_concurrent_unlock_or_session_switch() {
    for switched in [false, true] {
        let state = Arc::new(SessionState::default());
        state.locked.store(true, Ordering::SeqCst);
        let (_server, client) = fake_logind(state.clone()).await;
        let changed = state.clone();
        let result = perform_connected_with_validation(
            &client,
            "suspend",
            Duration::from_secs(1),
            false,
            || std::future::ready(Ok(())),
            move || {
                if switched {
                    changed.inactive.store(true, Ordering::SeqCst);
                } else {
                    changed.locked.store(false, Ordering::SeqCst);
                }
                std::future::ready(Ok(()))
            },
            &outcome::Tracker::default(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn dependency_deadline_cancels_pending_work() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = dropped.clone();
    let result = bounded("preflight", Duration::from_millis(10), async move {
        let _owned = Dropped(flag);
        std::future::pending::<Result<()>>().await
    })
    .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("preflight timed out")
    );
    assert!(dropped.load(Ordering::SeqCst));
}

#[derive(Default)]
pub(super) struct SessionState {
    locked: AtomicBool,
    pub(super) preparing: AtomicBool,
    pub(super) inhibit_calls: AtomicUsize,
    pub(super) inhibit_fails: AtomicBool,
    pub(super) inhibitor_peer: std::sync::Mutex<Option<std::os::unix::net::UnixStream>>,
    pub(super) keep_awake: AtomicBool,
    pub(super) telemetry_fails: AtomicBool,
    pub(super) telemetry_stalled: AtomicBool,
    pub(super) telemetry_started: Notify,
    pub(super) telemetry_release: Notify,
    pub(super) telemetry_queries: AtomicUsize,
    capability: Option<&'static str>,
    lock_calls: AtomicUsize,
    requested: Notify,
    sleep_calls: AtomicUsize,
    lock_fails: bool,
    hint_fails: bool,
    inactive: AtomicBool,
    pub(super) preparing_fails: AtomicBool,
    telemetry_fails_after_sleep: bool,
}

struct FakeAutoSession;

// Only identity resolution is allowed on the moving 'auto' alias. All locking
// and subsequent checks must use the concrete session object.
#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl FakeAutoSession {
    #[zbus(property)]
    fn id(&self) -> &str {
        "test"
    }
}

struct FakeSession(Arc<SessionState>);

#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl FakeSession {
    #[zbus(property)]
    fn id(&self) -> &str {
        "test"
    }

    #[zbus(property)]
    fn active(&self) -> bool {
        !self.0.inactive.load(Ordering::SeqCst)
    }

    fn lock(&self) -> zbus::fdo::Result<()> {
        self.0.lock_calls.fetch_add(1, Ordering::SeqCst);
        self.0.requested.notify_one();
        if self.0.lock_fails {
            return Err(zbus::fdo::Error::Failed("locker failed".into()));
        }
        Ok(())
    }

    #[zbus(property)]
    fn locked_hint(&self) -> zbus::fdo::Result<bool> {
        if self.0.hint_fails {
            return Err(zbus::fdo::Error::Failed("no lock confirmation".into()));
        }
        Ok(self.0.locked.load(Ordering::SeqCst))
    }
}

struct FakeManager(Arc<SessionState>);

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl FakeManager {
    fn get_session(&self, id: &str) -> zvariant::OwnedObjectPath {
        assert_eq!(id, "test");
        zvariant::OwnedObjectPath::try_from("/org/freedesktop/login1/session/test").unwrap()
    }

    async fn can_suspend(&self) -> &str {
        self.0.telemetry_queries.fetch_add(1, Ordering::SeqCst);
        if self.0.telemetry_stalled.load(Ordering::SeqCst) {
            self.0.telemetry_started.notify_one();
            self.0.telemetry_release.notified().await;
        }
        self.0.capability.unwrap_or("yes")
    }
    fn can_hibernate(&self) -> &str {
        self.0.capability.unwrap_or("yes")
    }
    fn can_suspend_then_hibernate(&self) -> &str {
        self.0.capability.unwrap_or("yes")
    }
    fn suspend_then_hibernate(&self, _interactive: bool) {
        self.0.sleep_calls.fetch_add(1, Ordering::SeqCst);
        self.0.preparing.store(true, Ordering::SeqCst);
    }
    fn inhibit(
        &self,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
    ) -> zbus::fdo::Result<zvariant::OwnedFd> {
        assert_eq!(what, "sleep:handle-lid-switch");
        assert_eq!(who, "Shelllist Keep awake");
        assert!(!why.is_empty());
        assert_eq!(mode, "block");
        if self.0.inhibit_fails.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::AccessDenied("inhibition denied".into()));
        }
        self.0.inhibit_calls.fetch_add(1, Ordering::SeqCst);
        self.0.keep_awake.store(true, Ordering::SeqCst);
        let (peer, fd) = std::os::unix::net::UnixStream::pair().unwrap();
        *self.0.inhibitor_peer.lock().unwrap() = Some(peer);
        Ok(std::os::fd::OwnedFd::from(fd).into())
    }
    fn list_inhibitors(&self) -> zbus::fdo::Result<Vec<RawInhibitor>> {
        if self.0.telemetry_fails.load(Ordering::SeqCst)
            || (self.0.telemetry_fails_after_sleep && self.0.sleep_calls.load(Ordering::SeqCst) > 0)
        {
            return Err(zbus::fdo::Error::Failed("telemetry disconnected".into()));
        }
        if self.0.keep_awake.load(Ordering::SeqCst) {
            return Ok(vec![(
                "sleep:handle-lid-switch".into(),
                "Shelllist Keep awake".into(),
                "test".into(),
                "block".into(),
                1000,
                std::process::id(),
            )]);
        }
        Ok(Vec::new())
    }
    #[zbus(property)]
    fn preparing_for_sleep(&self) -> zbus::fdo::Result<bool> {
        if self.0.preparing_fails.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed(
                "preparation state unavailable".into(),
            ));
        }
        Ok(self.0.preparing.load(Ordering::SeqCst))
    }
    fn suspend(&self, _interactive: bool) {
        self.0.sleep_calls.fetch_add(1, Ordering::SeqCst);
        self.0.preparing.store(true, Ordering::SeqCst);
    }
    fn hibernate(&self, _interactive: bool) {
        self.0.sleep_calls.fetch_add(1, Ordering::SeqCst);
        self.0.preparing.store(true, Ordering::SeqCst);
    }
}

pub(super) async fn fake_logind(state: Arc<SessionState>) -> (Connection, Connection) {
    let (server, client) = UnixStream::pair().unwrap();
    let server = Builder::unix_stream(server)
        .server(zbus::Guid::generate())
        .unwrap()
        .p2p()
        .serve_at(SESSION_PATH, FakeAutoSession)
        .unwrap()
        .serve_at(
            "/org/freedesktop/login1/session/test",
            FakeSession(Arc::clone(&state)),
        )
        .unwrap()
        .serve_at(MANAGER_PATH, FakeManager(state))
        .unwrap()
        .build();
    let client = Builder::unix_stream(client).p2p().build();
    let (server, client) = tokio::try_join!(server, client).unwrap();
    (server, client)
}

#[tokio::test]
async fn sleep_waits_for_lock_confirmation_not_just_the_method_reply() {
    for action in ["suspend", "hibernate", "suspend-then-hibernate"] {
        let state = Arc::new(SessionState::default());
        let (_server, client) = fake_logind(Arc::clone(&state)).await;
        let mut operation = tokio::spawn(async move {
            perform_connected(&client, action, Duration::from_secs(2)).await
        });
        timeout(Duration::from_secs(1), state.requested.notified())
            .await
            .unwrap();
        assert!(
            timeout(Duration::from_millis(30), &mut operation)
                .await
                .is_err()
        );
        assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0);
        state.locked.store(true, Ordering::SeqCst);
        timeout(Duration::from_secs(1), operation)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn missing_or_failed_lock_confirmation_never_sleeps() {
    for action in ["suspend", "hibernate", "suspend-then-hibernate"] {
        for (lock_fails, hint_fails) in [(false, false), (true, false), (false, true)] {
            let state = Arc::new(SessionState {
                lock_fails,
                hint_fails,
                ..Default::default()
            });
            let (_server, client) = fake_logind(Arc::clone(&state)).await;
            let result = timeout(
                Duration::from_secs(1),
                perform_connected(&client, action, Duration::from_millis(100)),
            )
            .await
            .unwrap();
            assert!(result.is_err());
            assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn already_preparing_sleep_rejects_duplicate_requests_before_locking() {
    for action in ["suspend", "hibernate", "suspend-then-hibernate"] {
        let state = Arc::new(SessionState::default());
        state.preparing.store(true, Ordering::SeqCst);
        let (_server, client) = fake_logind(Arc::clone(&state)).await;
        let result = perform_connected(&client, action, Duration::from_secs(1)).await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("already preparing")
        );
        assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.lock_calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn operation_result_preserves_loginds_actual_preparation_state() {
    let state = Arc::new(SessionState::default());
    state.locked.store(true, Ordering::SeqCst);
    let (_server, client) = fake_logind(state).await;
    let result = perform_connected(&client, "suspend", Duration::from_secs(1))
        .await
        .unwrap();
    assert!(
        result.preparing_for_sleep,
        "method completion must not clear a live preparation signal"
    );
}

#[tokio::test]
async fn simultaneous_public_requests_fail_without_contacting_the_system_bus() {
    let _pending = SLEEP_ACTION.lock().await;
    for action in ["lock", "suspend", "hibernate", "suspend-then-hibernate"] {
        let error = perform(action).await.unwrap_err();
        assert!(error.to_string().contains("already in progress"));
    }
}

#[tokio::test]
async fn unreadable_preparation_state_fails_closed_before_locking() {
    let state = Arc::new(SessionState::default());
    state.preparing_fails.store(true, Ordering::SeqCst);
    let (_server, client) = fake_logind(Arc::clone(&state)).await;
    let error = perform_connected(&client, "suspend", Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("cannot confirm"));
    assert_eq!(state.lock_calls.load(Ordering::SeqCst), 0);
    assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn changes_during_setup_are_rechecked_before_sleep() {
    for change in [
        "unlocked",
        "inactive",
        "preparing",
        "preparing-unreadable",
        "setup-failed",
        "keep-awake",
    ] {
        let state = Arc::new(SessionState::default());
        state.locked.store(true, Ordering::SeqCst);
        let (_server, client) = fake_logind(Arc::clone(&state)).await;
        let result = perform_connected_with_setup(
            &client,
            "suspend-then-hibernate",
            Duration::from_secs(1),
            false,
            || async {
                assert!(
                    state.locked.load(Ordering::SeqCst),
                    "setup must follow confirmed locking"
                );
                match change {
                    "unlocked" => state.locked.store(false, Ordering::SeqCst),
                    "inactive" => state.inactive.store(true, Ordering::SeqCst),
                    "preparing" => state.preparing.store(true, Ordering::SeqCst),
                    "keep-awake" => state.keep_awake.store(true, Ordering::SeqCst),
                    "preparing-unreadable" => state.preparing_fails.store(true, Ordering::SeqCst),
                    _ => bail!("helper rejected settings"),
                }
                Ok(())
            },
        )
        .await;
        assert!(result.is_err(), "{change}");
        assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0, "{change}");
    }
}

#[tokio::test]
async fn unsuccessful_lock_never_changes_hibernate_settings() {
    let state = Arc::new(SessionState::default());
    let (_server, client) = fake_logind(Arc::clone(&state)).await;
    let setup_calls = AtomicUsize::new(0);
    let result = perform_connected_with_setup(
        &client,
        "suspend-then-hibernate",
        Duration::from_millis(80),
        false,
        || async {
            setup_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    )
    .await;
    assert!(result.is_err());
    assert_eq!(setup_calls.load(Ordering::SeqCst), 0);
    assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn accepted_action_is_not_reported_failed_when_telemetry_breaks() {
    let state = Arc::new(SessionState {
        telemetry_fails_after_sleep: true,
        ..Default::default()
    });
    state.locked.store(true, Ordering::SeqCst);
    let (_server, client) = fake_logind(Arc::clone(&state)).await;
    let result = perform_connected(&client, "suspend", Duration::from_secs(1))
        .await
        .unwrap();
    assert!(!result.available);
    assert!(result.preparing_for_sleep);
    assert!(
        result
            .error
            .unwrap()
            .contains("accepted, but status refresh failed")
    );
    assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_actions_never_fall_through_to_hibernate() {
    let state = Arc::new(SessionState::default());
    let (_server, client) = fake_logind(Arc::clone(&state)).await;
    assert!(
        perform_connected(&client, "suspnd", Duration::from_secs(1))
            .await
            .is_err()
    );
    assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0);
    assert_eq!(state.lock_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn keep_awake_blocks_every_sleep_path_before_lock_or_setup_but_allows_lock() {
    let state = Arc::new(SessionState::default());
    state.keep_awake.store(true, Ordering::SeqCst);
    let (_server, client) = fake_logind(Arc::clone(&state)).await;
    for action in ["suspend", "hibernate", "suspend-then-hibernate"] {
        let error = perform_connected_with_setup(
            &client,
            action,
            Duration::from_secs(1),
            false,
            || async { panic!("blocked sleep must not change hibernate settings") },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("Keep awake"));
    }
    assert_eq!(state.lock_calls.load(Ordering::SeqCst), 0);
    assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0);
    state.locked.store(true, Ordering::SeqCst);
    assert!(
        perform_connected(&client, "lock", Duration::from_secs(1))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn already_locked_session_and_lock_only_action_are_supported() {
    let state = Arc::new(SessionState::default());
    state.locked.store(true, Ordering::SeqCst);
    let (_server, client) = fake_logind(Arc::clone(&state)).await;
    perform_connected(&client, "lock", Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 0);
    perform_connected(&client, "suspend", Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(state.sleep_calls.load(Ordering::SeqCst), 1);
}
