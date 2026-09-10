//! In-process logind substitute. Tests never connect to the real system bus.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use tokio::{net::UnixStream, sync::Notify, time::timeout};
use zbus::{Connection, connection::Builder};

use super::*;

#[derive(Default)]
struct SessionState {
    locked: AtomicBool,
    preparing: AtomicBool,
    lock_calls: AtomicUsize,
    requested: Notify,
    sleep_calls: AtomicUsize,
    lock_fails: bool,
    hint_fails: bool,
    inactive: AtomicBool,
    preparing_fails: AtomicBool,
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

    fn can_suspend(&self) -> &str {
        "yes"
    }
    fn can_hibernate(&self) -> &str {
        "yes"
    }
    fn can_suspend_then_hibernate(&self) -> &str {
        "yes"
    }
    fn suspend_then_hibernate(&self, _interactive: bool) {
        self.0.sleep_calls.fetch_add(1, Ordering::SeqCst);
        self.0.preparing.store(true, Ordering::SeqCst);
    }
    fn list_inhibitors(&self) -> zbus::fdo::Result<Vec<RawInhibitor>> {
        if self.0.telemetry_fails_after_sleep && self.0.sleep_calls.load(Ordering::SeqCst) > 0 {
            return Err(zbus::fdo::Error::Failed("telemetry disconnected".into()));
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

async fn fake_logind(state: Arc<SessionState>) -> (Connection, Connection) {
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
