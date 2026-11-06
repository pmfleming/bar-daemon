//! Read-only sleep transitions for display and activity processes. No power policy.
use crate::state::StateStore;
use anyhow::{Context, Result};
use futures::StreamExt;
use shelllist_daemon_tokio::{ResumeDetector, suspend_offset};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug)]
enum Event {
    Preparation(bool),
    Readback(bool),
}

pub(crate) async fn monitor(store: StateStore) {
    let (sender, receiver) = mpsc::channel(16);
    tokio::join!(signals(sender), observe(store, receiver));
}
async fn signals(sender: mpsc::Sender<Event>) {
    loop {
        if let Err(error) = connection(&sender).await {
            tracing::debug!(%error, "sleep observation reconnecting");
        }
        if sender.is_closed() {
            return;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
async fn connection(sender: &mpsc::Sender<Event>) -> Result<()> {
    let connection = crate::bus::system_owned().await?;
    let proxy = crate::sleep::manager(&connection).await?;
    let mut changes = proxy.receive_signal("PrepareForSleep").await?;
    // Subscribe before reading the current preparation state.
    sender
        .send(Event::Readback(
            proxy.get_property::<bool>("PreparingForSleep").await?,
        ))
        .await?;
    while let Some(message) = changes.next().await {
        let (preparing,): (bool,) = message
            .body()
            .deserialize()
            .context("decode sleep preparation")?;
        sender.send(Event::Preparation(preparing)).await?;
    }
    anyhow::bail!("sleep observation stream ended")
}
async fn observe(store: StateStore, mut events: mpsc::Receiver<Event>) {
    let mut detector = ResumeDetector::default();
    detector.poll(suspend_offset());
    let mut clock = tokio::time::interval(Duration::from_secs(2));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(Event::Preparation(true) | Event::Readback(true)) => store.record_sleep_preparation(true).await,
                Some(event @ (Event::Preparation(false) | Event::Readback(false))) => {
                    // Initial/reconnect readback must not invent a resume.
                    let preparing = store.read(|s| s.power_sleep.preparing_for_sleep).await;
                    if (matches!(event, Event::Preparation(false)) || preparing) && detector.signal(suspend_offset()) { store.record_resume().await; }
                    else { store.record_sleep_preparation(false).await; }
                }
                None => return,
            },
            _ = clock.tick() => if detector.poll(suspend_offset()) { store.record_resume().await; },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn initial_read_does_not_resume_and_sleep_edges_refresh_internal_observers() {
        let store = StateStore::default();
        let (sender, receiver) = mpsc::channel(8);
        let watcher = tokio::spawn(observe(store.clone(), receiver));
        sender.send(Event::Readback(false)).await.unwrap();
        sender.send(Event::Preparation(true)).await.unwrap();
        sender.send(Event::Preparation(false)).await.unwrap();
        drop(sender);
        watcher.await.unwrap();
        let state = store.snapshot().await.power_sleep;
        assert!(!state.preparing_for_sleep);
        assert_eq!(state.resume_generation, 1);
    }
}
