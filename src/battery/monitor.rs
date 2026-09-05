use std::{io, os::fd::AsRawFd, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use tokio::{io::unix::AsyncFd, sync::mpsc};

use super::sysfs::PowerSupplyFs;

pub(super) struct NativeMonitor {
    power_supply: PowerSupplyFs,
    events: mpsc::Receiver<()>,
    reconciliation: tokio::time::Interval,
}

impl NativeMonitor {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            power_supply: PowerSupplyFs::new(root),
            events: spawn_udev_monitor(),
            reconciliation: tokio::time::interval(Duration::from_secs(30)),
        }
    }

    pub(super) fn power_supply(&self) -> &PowerSupplyFs {
        &self.power_supply
    }

    pub(super) async fn changed(&mut self) {
        tokio::select! {
            event = self.events.recv() => {
                if event.is_none() {
                    self.reconciliation.tick().await;
                }
            }
            _ = self.reconciliation.tick() => {}
        }
    }
}

fn spawn_udev_monitor() -> mpsc::Receiver<()> {
    let (sender, receiver) = mpsc::channel(8);
    if let Err(error) = std::thread::Builder::new()
        .name("bar-battery-udev".into())
        .spawn(move || {
            if let Err(error) = run_udev_monitor(sender) {
                tracing::warn!(%error, "power-supply udev monitor stopped; polling remains active");
            }
        })
    {
        tracing::warn!(%error, "power-supply udev thread unavailable; polling remains active");
    }
    receiver
}

fn run_udev_monitor(sender: mpsc::Sender<()>) -> Result<()> {
    let socket = udev::MonitorBuilder::new()
        .context("create udev monitor")?
        .match_subsystem("power_supply")
        .context("filter power-supply udev events")?
        .listen()
        .context("listen for power-supply udev events")?;
    // udev's socket is nonblocking and not Send. Keep it on this thread,
    // using a local reactor rather than treating an empty iterator as EOF.
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .context("create power-supply event reactor")?
        .block_on(forward_ready_events(socket, sender, |socket| {
            socket.iter().count() > 0
        }))
}

async fn forward_ready_events<S: AsRawFd>(
    socket: S,
    sender: mpsc::Sender<()>,
    mut drain: impl FnMut(&S) -> bool,
) -> Result<()> {
    let socket = AsyncFd::new(socket).context("register power-supply event socket")?;
    loop {
        let mut ready = tokio::select! {
            _ = sender.closed() => return Ok(()),
            ready = socket.readable() => ready.context("wait for power-supply events")?,
        };
        let Ok(result) = ready.try_io(|socket| {
            if drain(socket.get_ref()) {
                Ok(())
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            }
        }) else {
            continue;
        };
        result.context("read power-supply events")?;
        // A burst only needs one refresh. Stop promptly when the monitor drops.
        if sender.send(()).await.is_err() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{io::ErrorKind, os::unix::net::UnixDatagram};

    use super::*;

    fn drain(socket: &UnixDatagram) -> bool {
        let mut changed = false;
        loop {
            match socket.recv(&mut [0u8; 1]) {
                Ok(_) => changed = true,
                Err(error) if error.kind() == ErrorKind::WouldBlock => return changed,
                Err(error) => panic!("read test event: {error}"),
            }
        }
    }

    #[tokio::test]
    async fn readiness_monitor_survives_idle_periods_and_stops_on_drop() -> Result<()> {
        let (producer, socket) = UnixDatagram::pair()?;
        socket.set_nonblocking(true)?;
        let (sender, mut events) = mpsc::channel(8);
        let task = tokio::spawn(forward_ready_events(socket, sender, drain));
        for _ in 0..2 {
            assert!(
                tokio::time::timeout(Duration::from_millis(20), events.recv())
                    .await
                    .is_err(),
                "an idle socket must neither publish nor close the stream"
            );
            producer.send(b"a")?;
            producer.send(b"b")?;
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), events.recv()).await?,
                Some(())
            );
        }
        // Both datagrams in each burst were drained into a single refresh.
        assert!(events.try_recv().is_err());
        drop(events);
        tokio::time::timeout(Duration::from_secs(1), task).await???;
        Ok(())
    }
}
