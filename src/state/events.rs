//! Domain-local delivery. Full state is replaceable; operation/edge-bearing
//! streams retain bounded history. There is deliberately no all-domain ring.
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use super::DomainEvent;
use broadcast::error::{RecvError, TryRecvError};
use tokio::sync::{broadcast, watch};

#[derive(Default)]
struct Metrics {
    events: AtomicU64,
    bytes: AtomicU64,
    lag: AtomicU64,
    coalesced: AtomicU64,
}

pub(super) struct Source {
    channel: Channel,
    metrics: Arc<Metrics>,
}
enum Channel {
    State(watch::Sender<(u64, DomainEvent)>),
    Transitions(broadcast::Sender<DomainEvent>),
}

pub(super) fn ordered(stream: &str) -> bool {
    use crate::protocol::stream::{BATTERY, NOTIFICATION_ACTIVE, OSD_HARDWARE, POWER_SLEEP};
    matches!(
        stream,
        POWER_SLEEP | BATTERY | NOTIFICATION_ACTIVE | OSD_HARDWARE
    )
}

impl Source {
    pub(super) fn new(stream: &str) -> Self {
        let channel = if ordered(stream) {
            Channel::Transitions(broadcast::channel(128).0)
        } else {
            Channel::State(
                watch::channel((
                    0,
                    DomainEvent {
                        stream: stream.into(),
                        data: serde_json::Value::Null,
                    },
                ))
                .0,
            )
        };
        Self {
            channel,
            metrics: Arc::default(),
        }
    }
    pub(super) fn send(&self, event: DomainEvent) {
        let revision = self.metrics.events.fetch_add(1, Ordering::Relaxed) + 1;
        let bytes = event.data.to_string().len() as u64;
        let payload_bytes_total = self.metrics.bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
        tracing::trace!(stream = %event.stream, events_total = revision, payload_bytes = bytes,
            payload_bytes_total, "bar domain publication");
        match &self.channel {
            Channel::State(sender) => {
                sender.send_replace((revision, event));
            }
            Channel::Transitions(sender) => {
                let _ = sender.send(event);
            }
        }
    }
    pub(super) fn subscribe(&self) -> DomainReceiver {
        let channel = match &self.channel {
            Channel::State(sender) => {
                let receiver = sender.subscribe();
                let revision = receiver.borrow().0;
                Receiver::State(receiver, revision)
            }
            Channel::Transitions(sender) => Receiver::Transitions(sender.subscribe()),
        };
        DomainReceiver {
            channel,
            metrics: self.metrics.clone(),
        }
    }
}

enum Receiver {
    State(watch::Receiver<(u64, DomainEvent)>, u64),
    Transitions(broadcast::Receiver<DomainEvent>),
}
pub(crate) struct DomainReceiver {
    channel: Receiver,
    metrics: Arc<Metrics>,
}
impl DomainReceiver {
    pub(crate) async fn recv(&mut self) -> Result<DomainEvent, RecvError> {
        match &mut self.channel {
            Receiver::State(receiver, previous) => {
                receiver.changed().await.map_err(|_| RecvError::Closed)?;
                // changed marks seen, but borrow_and_update below still reads
                // the latest value atomically and advances the metric cursor.
                Ok(self.metrics.current(receiver, previous))
            }
            Receiver::Transitions(receiver) => {
                let result = receiver.recv().await;
                if let Err(RecvError::Lagged(count)) = result {
                    self.metrics.lag.fetch_add(count, Ordering::Relaxed);
                }
                result
            }
        }
    }
    pub(crate) fn try_recv(&mut self) -> Result<DomainEvent, TryRecvError> {
        match &mut self.channel {
            Receiver::State(receiver, previous) => match receiver.has_changed() {
                Ok(true) => Ok(self.metrics.current(receiver, previous)),
                Ok(false) => Err(TryRecvError::Empty),
                Err(_) => Err(TryRecvError::Closed),
            },
            Receiver::Transitions(receiver) => {
                let result = receiver.try_recv();
                if let Err(TryRecvError::Lagged(count)) = result {
                    self.metrics.lag.fetch_add(count, Ordering::Relaxed);
                }
                result
            }
        }
    }
}

impl Metrics {
    fn current(
        &self,
        receiver: &mut watch::Receiver<(u64, DomainEvent)>,
        previous: &mut u64,
    ) -> DomainEvent {
        let (revision, event) = &*receiver.borrow_and_update();
        let skipped = revision.saturating_sub(*previous).saturating_sub(1);
        let coalesced_total = self.coalesced.fetch_add(skipped, Ordering::Relaxed) + skipped;
        if skipped > 0 {
            tracing::debug!(stream = %event.stream, skipped, coalesced_total, "bar replaceable state coalesced");
        }
        *previous = *revision;
        event.clone()
    }
}

/// Used by offline fixtures to observe several independent sources. Production
/// monitors request one domain; D-Bus forwarding owns one worker per domain.
pub(crate) struct DomainReceivers(pub(crate) Vec<DomainReceiver>);
impl DomainReceivers {
    pub(crate) async fn recv(&mut self) -> Result<DomainEvent, RecvError> {
        if self.0.is_empty() {
            return std::future::pending().await;
        }
        futures::future::select_all(self.0.iter_mut().map(|receiver| Box::pin(receiver.recv())))
            .await
            .0
    }
    pub(crate) fn try_recv(&mut self) -> Result<DomainEvent, TryRecvError> {
        for receiver in &mut self.0 {
            match receiver.try_recv() {
                Err(TryRecvError::Empty) => {}
                result => return result,
            }
        }
        Err(TryRecvError::Empty)
    }
}
