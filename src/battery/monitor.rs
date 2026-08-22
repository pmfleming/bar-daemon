use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use tokio::sync::mpsc;

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
    for _event in socket.iter() {
        if sender.blocking_send(()).is_err() {
            return Ok(());
        }
    }
    Ok(())
}
