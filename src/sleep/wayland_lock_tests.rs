//! An isolated protocol server; tests never use the desktop display.
use super::{Connection, Duration, LockObserver, State, WaylandError};
use tokio::sync::{mpsc, oneshot};
use wayland_server::{Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New};

mod server_protocol {
    // Generated child modules explicitly refer to super::wayland_server.
    #[allow(clippy::single_component_path_imports)]
    use wayland_server;
    pub mod __interfaces {
        use wayland_server::backend as wayland_backend;
        wayland_scanner::generate_interfaces!("protocols/hyprland-lock-notify-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("protocols/hyprland-lock-notify-v1.xml");
}
use server_protocol::{
    hyprland_lock_notification_v1::{
        self as notification, HyprlandLockNotificationV1 as Notification,
    },
    hyprland_lock_notifier_v1::{self as manager, HyprlandLockNotifierV1 as Manager},
};

struct Server {
    locked: bool,
    notifications: Vec<Notification>,
}

impl GlobalDispatch<Manager, ()> for Server {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<Manager>,
        _: &(),
        data: &mut DataInit<'_, Self>,
    ) {
        data.init(resource, ());
    }
}

impl Dispatch<Manager, ()> for Server {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &Manager,
        request: manager::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let manager::Request::GetLockNotification { id } = request {
            let notification = data.init(id, ());
            if state.locked {
                notification.locked();
            }
            state.notifications.push(notification);
        }
    }
}

impl Dispatch<Notification, ()> for Server {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &Notification,
        _: notification::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

struct FakeCompositor {
    task: tokio::task::JoinHandle<()>,
    commands: mpsc::UnboundedSender<(bool, oneshot::Sender<()>)>,
}

impl Drop for FakeCompositor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeCompositor {
    fn start(advertise: bool, locked: bool) -> (Self, Connection) {
        let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut display = Display::<Server>::new().unwrap();
        let mut handle = display.handle();
        if advertise {
            handle.create_global::<Server, Manager, _>(1, ());
        }
        handle
            .insert_client(server, std::sync::Arc::new(()))
            .unwrap();
        let (commands, mut receiver) = mpsc::unbounded_channel::<(bool, oneshot::Sender<()>)>();
        let task = tokio::spawn(async move {
            let mut state = Server {
                locked,
                notifications: Vec::new(),
            };
            loop {
                display.dispatch_clients(&mut state).unwrap();
                while let Ok((locked, done)) = receiver.try_recv() {
                    state.locked = locked;
                    for notification in &state.notifications {
                        if locked {
                            notification.locked();
                        } else {
                            notification.unlocked();
                        }
                    }
                    display.flush_clients().unwrap();
                    let _ = done.send(());
                }
                display.flush_clients().unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        (
            Self { task, commands },
            Connection::from_socket(client).unwrap(),
        )
    }

    async fn set_locked(&self, locked: bool) {
        let (done, wait) = oneshot::channel();
        self.commands.send((locked, done)).unwrap();
        wait.await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn stalled_reads_and_write_backpressure_share_the_confirmation_deadline() {
    for congested in [false, true] {
        // Keep the peer open without ever reading requests or answering syncs.
        let (_server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = Connection::from_socket(client).unwrap();
        let queue = connection.new_event_queue::<State>();
        if congested {
            let blocked = (0..65_536).any(|_| {
                connection.display().sync(&queue.handle(), ());
                matches!(connection.flush(), Err(WaylandError::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock)
            });
            assert!(blocked, "fixture must actually exercise write backpressure");
        }
        let error = LockObserver::from_connection(connection)
            .await
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("lock confirmation timed out"),
            "{error:#}"
        );
    }
}

#[tokio::test]
async fn observes_actual_lock_completion_and_does_not_reuse_an_unlocked_hint() {
    for (supported, initially_locked) in [(false, false), (true, false), (true, true)] {
        let (server, connection) = FakeCompositor::start(supported, initially_locked);
        let observer = LockObserver::from_connection(connection).await;
        if !supported {
            assert!(observer.is_err(), "missing protocol must fail closed");
            continue;
        }
        let mut observer = observer.unwrap();
        assert_eq!(observer.locked().await.unwrap(), initially_locked);
        server.set_locked(true).await;
        assert!(observer.locked().await.unwrap());
        server.set_locked(false).await;
        assert!(
            !observer.locked().await.unwrap(),
            "the final pre-sleep check must drain unlock events"
        );
        server.set_locked(true).await;
        assert!(observer.locked().await.unwrap());
        server.task.abort();
        while !server.task.is_finished() {
            tokio::task::yield_now().await;
        }
        assert!(observer.locked().await.is_err());
    }
}
