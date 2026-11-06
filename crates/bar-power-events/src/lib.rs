//! Udev power-supply events, isolated from policy and other session binaries.
use anyhow::{Context, Result};
use std::{io, os::fd::AsRawFd};
use tokio::{io::unix::AsyncFd, sync::mpsc};
pub fn events() -> mpsc::Receiver<()> {
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
