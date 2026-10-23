use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct Activation {
    supported: AtomicBool,
    fail: AtomicBool,
    calls: AtomicUsize,
}
struct Root(Arc<Activation>);

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        self.0.supported.load(Ordering::SeqCst)
    }

    fn raise(&self) -> zbus::fdo::Result<()> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if self.0.fail.load(Ordering::SeqCst) {
            Err(zbus::fdo::Error::Failed("activation refused".into()))
        } else {
            Ok(())
        }
    }
}

struct Player;
#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    #[zbus(property)]
    fn can_control(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn raise_uses_root_capability_without_playback_and_propagates_failure() {
    let activation = Arc::new(Activation::default());
    activation.supported.store(true, Ordering::SeqCst);
    let (_server, connection) = crate::test_support::dbus_peer(|builder| {
        builder
            .serve_at(PATH, Root(activation.clone()))?
            .serve_at(PATH, Player)
    })
    .await
    .unwrap();
    let service = MediaService::default();
    *service.connection.write().await = Some(connection.clone());
    let id = "org.mpris.MediaPlayer2.browser";
    let snapshot = read_player(&connection, id).await.unwrap();
    assert!(snapshot.can_raise);
    assert!(!snapshot.can_control);
    assert_eq!(service.operation(Some(id), "raise").await.unwrap(), id);
    assert_eq!(activation.calls.load(Ordering::SeqCst), 1);

    activation.supported.store(false, Ordering::SeqCst);
    assert!(!read_player(&connection, id).await.unwrap().can_raise);
    assert!(service.operation(Some(id), "raise").await.is_err());
    assert_eq!(activation.calls.load(Ordering::SeqCst), 1);

    activation.supported.store(true, Ordering::SeqCst);
    activation.fail.store(true, Ordering::SeqCst);
    assert!(service.operation(Some(id), "raise").await.is_err());
    assert_eq!(activation.calls.load(Ordering::SeqCst), 2, "no retry");
    assert!(service.operation(Some("invalid"), "raise").await.is_err());
    assert_eq!(activation.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn missing_raise_capability_fails_closed() {
    let (_server, connection) =
        crate::test_support::dbus_peer(|builder| builder.serve_at(PATH, Player))
            .await
            .unwrap();
    let id = "org.mpris.MediaPlayer2.legacy";
    assert!(!read_player(&connection, id).await.unwrap().can_raise);
    assert!(raise_player(&connection, id).await.is_err());
    let mut old_snapshot = serde_json::to_value(MediaPlayer::default()).unwrap();
    old_snapshot.as_object_mut().unwrap().remove("can_raise");
    assert!(
        !serde_json::from_value::<MediaPlayer>(old_snapshot)
            .unwrap()
            .can_raise
    );
}
