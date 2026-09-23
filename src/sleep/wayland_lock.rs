//! Observe lock completion, never acquire a lock or synthesize LockedHint.
//! The protocol sends Locked only after the compositor's transition barrier.
use std::{
    io,
    os::fd::{AsFd, OwnedFd},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::io::unix::AsyncFd;
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    backend::WaylandError,
    delegate_noop,
    protocol::{wl_callback, wl_registry},
};

mod protocol {
    // Generated child modules explicitly refer to super::wayland_client.
    #[allow(clippy::single_component_path_imports)]
    use wayland_client;
    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        wayland_scanner::generate_interfaces!("protocols/hyprland-lock-notify-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/hyprland-lock-notify-v1.xml");
}
use protocol::{
    hyprland_lock_notification_v1::{self, HyprlandLockNotificationV1},
    hyprland_lock_notifier_v1::HyprlandLockNotifierV1,
};

const ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(test)]
#[path = "wayland_lock_tests.rs"]
mod tests;

#[derive(Default)]
struct State {
    notification: Option<HyprlandLockNotificationV1>,
    global: Option<u32>,
    synced: bool,
    locked: bool,
    valid: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name, interface, ..
            } if interface == "hyprland_lock_notifier_v1" && state.global.is_none() => {
                let manager: HyprlandLockNotifierV1 = registry.bind(name, 1, qh, ());
                state.notification = Some(manager.get_lock_notification(qh, ()));
                state.global = Some(name);
                state.valid = true;
                manager.destroy();
            }
            wl_registry::Event::GlobalRemove { name } if state.global == Some(name) => {
                state.valid = false;
                state.locked = false;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.synced = true;
    }
}

delegate_noop!(State: ignore HyprlandLockNotifierV1);

impl Dispatch<HyprlandLockNotificationV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &HyprlandLockNotificationV1,
        event: hyprland_lock_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            hyprland_lock_notification_v1::Event::Locked => state.locked = true,
            hyprland_lock_notification_v1::Event::Unlocked => state.locked = false,
        }
    }
}

pub(super) struct LockObserver {
    connection: Connection,
    socket: AsyncFd<OwnedFd>,
    queue: EventQueue<State>,
    state: State,
}

impl LockObserver {
    pub(super) async fn connect() -> Result<Self> {
        let connection = Connection::connect_to_env()
            .context("connect to the session Wayland display for lock confirmation")?;
        Self::from_connection(connection).await
    }

    async fn from_connection(connection: Connection) -> Result<Self> {
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let socket = AsyncFd::new(connection.as_fd().try_clone_to_owned()?)?;
        let mut observer = Self {
            connection,
            socket,
            queue,
            state: State::default(),
        };
        // First sync discovers globals. The second processes the newly bound
        // notification and its initial Locked event, if already locked.
        observer.roundtrip().await?;
        if observer.state.notification.is_none() {
            bail!(
                "compositor does not support hyprland-lock-notify-v1; refusing unconfirmed sleep"
            );
        }
        observer.roundtrip().await?;
        Ok(observer)
    }

    pub(super) async fn locked(&mut self) -> Result<bool> {
        // Drain and synchronize again before every decision, including the final
        // pre-sleep check. A previously observed Locked event is not timeless.
        self.roundtrip().await?;
        if !self.state.valid {
            bail!("compositor lock notification became unavailable");
        }
        Ok(self.state.locked)
    }

    async fn roundtrip(&mut self) -> Result<()> {
        tokio::time::timeout(ROUNDTRIP_TIMEOUT, async {
            self.state.synced = false;
            self.connection.display().sync(&self.queue.handle(), ());
            loop {
                self.queue.dispatch_pending(&mut self.state)?;
                if self.state.synced {
                    return Ok(());
                }
                loop {
                    match self.connection.flush() {
                        Ok(()) => break,
                        Err(WaylandError::Io(error))
                            if error.kind() == io::ErrorKind::WouldBlock =>
                        {
                            self.socket.writable().await?.clear_ready();
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                let Some(read) = self.queue.prepare_read() else {
                    continue;
                };
                let mut ready = self.socket.readable().await?;
                if let Ok(result) = ready.try_io(|_| {
                    read.read().map_err(|error| match error {
                        WaylandError::Io(error) => error,
                        error => io::Error::other(error),
                    })
                }) {
                    result?;
                }
            }
        })
        .await
        .context("compositor lock confirmation timed out; refusing to sleep")?
    }
}
