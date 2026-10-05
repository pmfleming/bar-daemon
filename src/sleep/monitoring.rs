//! Keep transition detection independent of slow capability/inhibitor queries.
//! Signal, clock, telemetry and operation-outcome futures share one cancellation owner.
use std::{future::Future, time::Duration};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use tokio::{
    sync::{mpsc, watch},
    time::{MissedTickBehavior, interval, sleep, timeout},
};

use super::{manager, property_changes, read_state, resume};
use crate::{model::PowerSleepState, state::StateStore};

const CLOCK_POLL: Duration = Duration::from_secs(2);
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
enum Event {
    Prepare(bool),
    Refresh,
}

pub(crate) async fn monitor(store: StateStore) {
    let (events, receiver) = mpsc::channel(32);
    // A watch notification coalesces refresh requests while one query is in
    // flight. No unbounded queue or per-signal telemetry task is created.
    let (refresh, requests) = watch::channel(());
    tokio::join!(
        super::outcome::monitor(store.clone()),
        signal_connections(events),
        observe(
            &store,
            receiver,
            refresh,
            resume::suspend_offset,
            CLOCK_POLL
        ),
        refresh_loop(&store, requests, REFRESH_TIMEOUT, |preparing| async move {
            let connection = zbus::Connection::system().await?;
            read_state(&connection, preparing).await
        }),
    );
}

async fn signal_connections(events: mpsc::Sender<Event>) {
    while !events.is_closed() {
        let result = async {
            let connection = zbus::Connection::system().await?;
            signal_connection(&connection, &events).await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, "logind sleep signals unavailable; clock detection remains active");
        }
        if events.send(Event::Refresh).await.is_err() {
            return;
        }
        sleep(Duration::from_secs(3)).await;
    }
}

async fn signal_connection(
    connection: &zbus::Connection,
    events: &mpsc::Sender<Event>,
) -> Result<()> {
    let proxy = manager(connection).await?;
    let mut prepare = proxy.receive_signal("PrepareForSleep").await?;
    let mut changes = property_changes(&proxy).await?;
    events.send(Event::Refresh).await?;
    loop {
        let event = tokio::select! {
            signal = prepare.next() => {
                let Some(signal) = signal else { bail!("logind sleep signal stream ended"); };
                let (preparing,): (bool,) = signal.body().deserialize()
                    .context("decode logind PrepareForSleep signal")?;
                Event::Prepare(preparing)
            }
            signal = changes.next() => {
                if signal.is_none() { bail!("logind property stream ended"); }
                Event::Refresh
            }
            _ = events.closed() => return Ok(()),
        };
        events.send(event).await?;
    }
}

async fn observe<F>(
    store: &StateStore,
    mut events: mpsc::Receiver<Event>,
    refresh: watch::Sender<()>,
    mut sample: F,
    poll_interval: Duration,
) where
    F: FnMut() -> Option<i128>,
{
    let mut resumes = resume::ResumeDetector::default();
    // Establish the clock baseline before waiting for D-Bus setup or signals.
    resumes.poll(sample());
    let mut clock = interval(poll_interval);
    clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut fallback = interval(REFRESH_INTERVAL);
    fallback.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(Event::Prepare(preparing)) => {
                    super::outcome::TRACKER.prepared(preparing);
                    if !preparing && resumes.signal(sample()) {
                        store.record_resume().await;
                    } else {
                        store.record_sleep_preparation(preparing).await;
                    }
                    refresh.send_replace(());
                }
                Some(Event::Refresh) => { refresh.send_replace(()); }
                None => return,
            },
            _ = clock.tick() => {
                if resumes.poll(sample()) {
                    store.record_resume().await;
                    refresh.send_replace(());
                }
            }
            _ = fallback.tick() => { refresh.send_replace(()); }
        }
    }
}

async fn refresh_loop<F, Fut>(
    store: &StateStore,
    mut requests: watch::Receiver<()>,
    deadline: Duration,
    read: F,
) where
    F: Fn(bool) -> Fut,
    Fut: Future<Output = Result<PowerSleepState>>,
{
    while requests.changed().await.is_ok() {
        requests.borrow_and_update();
        let before = store.read(|s| s.power_sleep.clone()).await;
        let result = timeout(deadline, read(before.preparing_for_sleep))
            .await
            .context("sleep status refresh timed out")
            .and_then(std::convert::identity);
        let next = result.unwrap_or_else(|error| PowerSleepState {
            error: Some(format!("{error:#}")),
            preparing_for_sleep: before.preparing_for_sleep,
            lock_before_sleep: true,
            ..PowerSleepState::default()
        });
        // A query started before a resume/preparation/toggle must never roll
        // that newer state back, even if its old reply arrives afterwards.
        if !store.update_power_sleep_if_unchanged(next, &before).await {
            requests.mark_changed();
        }
    }
}

#[cfg(test)]
#[path = "monitoring_tests.rs"]
mod tests;
