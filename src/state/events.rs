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
    use crate::protocol::stream::*;
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
            Receiver::State(receiver, _) => {
                receiver.changed().await.map_err(|_| RecvError::Closed)?;
                // changed marks seen, but borrow_and_update below still reads
                // the latest value atomically and advances the metric cursor.
                Ok(self.current())
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
    fn current(&mut self) -> DomainEvent {
        let Receiver::State(receiver, previous) = &mut self.channel else {
            unreachable!()
        };
        let (revision, event) = &*receiver.borrow_and_update();
        let skipped = revision.saturating_sub(*previous).saturating_sub(1);
        let coalesced_total =
            self.metrics.coalesced.fetch_add(skipped, Ordering::Relaxed) + skipped;
        if skipped > 0 {
            tracing::debug!(stream = %event.stream, skipped, coalesced_total, "bar replaceable state coalesced");
        }
        *previous = *revision;
        event.clone()
    }
    pub(crate) fn try_recv(&mut self) -> Result<DomainEvent, TryRecvError> {
        match &mut self.channel {
            Receiver::State(receiver, _) => match receiver.has_changed() {
                Ok(true) => Ok(self.current()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn deterministic_shared_ring_baseline_and_domain_isolation() {
        let (old, mut baseline) = broadcast::channel(128);
        let audio = Source::new(crate::protocol::stream::AUDIO);
        let battery = Source::new(crate::protocol::stream::BATTERY);
        let notifications = Source::new(crate::protocol::stream::NOTIFICATION_ACTIVE);
        let mut audio_rx = audio.subscribe();
        let mut battery_rx = battery.subscribe();
        let mut notification_rx = notifications.subscribe();
        for n in 0..1024 {
            let event = DomainEvent {
                stream: "audio.changed".into(),
                data: json!({"volume_percent": n}),
            };
            old.send(event.clone()).unwrap();
            audio.send(event);
        }
        assert!(matches!(
            baseline.try_recv(),
            Err(TryRecvError::Lagged(896))
        ));
        assert_eq!(audio_rx.recv().await.unwrap().data["volume_percent"], 1023);
        assert_eq!(audio.metrics.coalesced.load(Ordering::Relaxed), 1023);
        assert_eq!(audio.metrics.events.load(Ordering::Relaxed), 1024);
        assert!(audio.metrics.bytes.load(Ordering::Relaxed) > 1024);
        assert!(matches!(battery_rx.try_recv(), Err(TryRecvError::Empty)));
        assert!(matches!(
            notification_rx.try_recv(),
            Err(TryRecvError::Empty)
        ));
        for n in 0..256 {
            battery.send(DomainEvent {
                stream: "battery.changed".into(),
                data: json!(n),
            });
        }
        assert!(matches!(
            battery_rx.recv().await,
            Err(RecvError::Lagged(128))
        ));
        assert_eq!(battery.metrics.lag.load(Ordering::Relaxed), 128);
        assert!(matches!(
            notification_rx.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }
}
