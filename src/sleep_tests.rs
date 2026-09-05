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
    requested: Notify,
    sleep_calls: AtomicUsize,
    lock_fails: bool,
    hint_fails: bool,
}

struct FakeSession(Arc<SessionState>);

#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl FakeSession {
    fn lock(&self) -> zbus::fdo::Result<()> {
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
    fn can_suspend(&self) -> &str {
        "yes"
    }
    fn can_hibernate(&self) -> &str {
        "yes"
    }
    fn list_inhibitors(&self) -> Vec<RawInhibitor> {
        Vec::new()
    }
    fn suspend(&self, _interactive: bool) {
        self.0.sleep_calls.fetch_add(1, Ordering::SeqCst);
    }
    fn hibernate(&self, _interactive: bool) {
        self.0.sleep_calls.fetch_add(1, Ordering::SeqCst);
    }
}

async fn fake_logind(state: Arc<SessionState>) -> (Connection, Connection) {
    let (server, client) = UnixStream::pair().unwrap();
    let server = Builder::unix_stream(server)
        .server(zbus::Guid::generate())
        .unwrap()
        .p2p()
        .serve_at(SESSION_PATH, FakeSession(Arc::clone(&state)))
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
    for action in ["suspend", "hibernate"] {
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
    for action in ["suspend", "hibernate"] {
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
