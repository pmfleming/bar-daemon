//! Presentation privacy is separate from user DND. Detection never changes DND
//! preferences and unknown observation never means a previously active share ended.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Sharing {
    pub available: bool,
    pub active: bool,
    pub prompt: bool,
    pub generation: u64,
    pub until_unix_ms: Option<u64>,
    pub until_sharing_ends: bool,
}
impl Sharing {
    pub(super) fn suppressed(&self, now: u64) -> bool {
        self.prompt
            || self.active && self.until_sharing_ends
            || self.until_unix_ms.is_some_and(|until| until > now)
    }
    pub(super) fn observe(&mut self, observed: Option<bool>) {
        self.available = observed.is_some();
        let Some(active) = observed else {
            return;
        };
        if active && !self.active {
            self.generation = self.generation.wrapping_add(1);
            self.prompt = true;
            self.until_sharing_ends = true;
        } else if !active {
            self.prompt = false;
            self.until_sharing_ends = false;
        }
        self.active = active;
    }
    pub(super) fn snooze(&mut self, generation: u64, minutes: u16, now: u64) -> Result<()> {
        ensure!(
            self.active && self.generation == generation,
            "Screen-sharing session changed"
        );
        ensure!(
            [0, 15, 60].contains(&minutes),
            "Choose 15 minutes, 1 hour or until sharing ends"
        );
        self.prompt = false;
        self.until_sharing_ends = minutes == 0;
        self.until_unix_ms = (minutes > 0).then(|| now.saturating_add(u64::from(minutes) * 60_000));
        Ok(())
    }
    pub(super) fn expire(&mut self, now: u64) {
        super::model::take_due(&mut self.until_unix_ms, now);
    }
}

pub(super) async fn monitor(
    engine: Arc<super::engine::NotificationEngine>,
    mut receiver: watch::Receiver<Option<bool>>,
) {
    while receiver.changed().await.is_ok() {
        let observed = *receiver.borrow_and_update();
        engine.observe_sharing(observed).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sharing_snooze_is_session_fenced_and_does_not_treat_unknown_as_stopped() {
        let mut sharing = Sharing::default();
        sharing.observe(Some(true));
        assert!(sharing.prompt && sharing.suppressed(100));
        sharing.observe(None);
        assert!(!sharing.available && sharing.suppressed(100));
        assert!(sharing.snooze(0, 15, 100).is_err());
        sharing.snooze(1, 15, 100).unwrap();
        assert!(!sharing.prompt && sharing.suppressed(899_999));
        sharing.expire(900_100);
        assert!(!sharing.suppressed(900_100));
        sharing.observe(Some(false));
        sharing.observe(Some(true));
        assert_eq!(sharing.generation, 2);
        sharing.snooze(2, 0, 1_000_000).unwrap();
        assert!(sharing.suppressed(u64::MAX));
        sharing.observe(Some(false));
        assert!(!sharing.suppressed(u64::MAX));
    }
}
