use std::{path::PathBuf, time::Duration};

use tokio::sync::mpsc;

use super::sysfs::PowerSupplyFs;

pub(super) struct NativeMonitor {
    power_supply: PowerSupplyFs,
    events: mpsc::Receiver<()>,
    reconciliation: tokio::time::Interval,
}

impl NativeMonitor {
    pub(super) fn new(root: PathBuf, events: mpsc::Receiver<()>) -> Self {
        Self {
            power_supply: PowerSupplyFs::new(root),
            events,
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
