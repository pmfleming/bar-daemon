//! Read-only bar-api v1 envelopes, shared by live dispatch and contract fixtures.
use serde::Serialize;
use serde_json::Value;
use shelllist_daemon_core::{ApiIdentity, Correlation, event_envelope};

use super::{NAME, VERSION, stream};
use crate::{api, model::BarSnapshot, state::DomainEvent};

#[derive(Serialize)]
struct SnapshotReply<'a> {
    snapshot: &'a BarSnapshot,
}

pub(crate) fn snapshot(snapshot: &BarSnapshot) -> Value {
    api::success(
        serde_json::to_value(SnapshotReply { snapshot }).expect("serializable bar snapshot"),
    )
}

#[derive(Serialize)]
struct Payload<T> {
    data: T,
}

#[derive(Serialize)]
struct Lagged {
    missed: u64,
}

enum Kind {
    Subscribed,
    Changed,
    Lagged,
}
impl Kind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Subscribed => "subscribed",
            Self::Changed => "changed",
            Self::Lagged => "lagged",
        }
    }
}

/// The signal stream and envelope stream come from the same constructor.
pub(crate) struct Event {
    stream: String,
    value: Value,
}

impl Event {
    fn new(stream: String, kind: Kind, subscription_id: &str, data: impl Serialize) -> Self {
        let value = event_envelope(
            ApiIdentity::new(NAME, VERSION as u32),
            &stream,
            kind.as_str(),
            Correlation::Subscription(subscription_id),
            serde_json::to_value(Payload { data }).expect("serializable bar event"),
        );
        Self { stream, value }
    }

    pub(crate) fn subscribed(stream: &str, subscription_id: &str, snapshot: &BarSnapshot) -> Self {
        Self::new(
            stream.into(),
            Kind::Subscribed,
            subscription_id,
            initial_stream_data(stream, snapshot),
        )
    }

    pub(crate) fn changed(subscription_id: &str, event: DomainEvent) -> Self {
        Self::new(event.stream, Kind::Changed, subscription_id, event.data)
    }

    pub(crate) fn lagged(stream: &str, subscription_id: &str, missed: u64) -> Self {
        Self::new(
            stream.into(),
            Kind::Lagged,
            subscription_id,
            Lagged { missed },
        )
    }

    pub(crate) fn stream(&self) -> &str {
        &self.stream
    }
    pub(crate) fn value(&self) -> &Value {
        &self.value
    }
    pub(crate) fn into_value(self) -> Value {
        self.value
    }
}

pub(crate) fn snapshot_key(stream: &str) -> &'static str {
    match stream {
        stream::ACTIVITY => "activity",
        stream::WORKAREA => "workarea",
        stream::COMPOSITOR => "compositor",
        stream::WORKSPACES => "workspaces",
        stream::MEDIA => "media",
        stream::AUDIO => "audio",
        stream::BRIGHTNESS => "brightness",
        stream::BATTERY => "battery",
        stream::POWER_PROFILE => "power_profile",
        stream::POWER_SLEEP => "power_sleep",
        stream::SLEEP_POLICY => "sleep_policy",
        stream::DISPLAY_POLICY => "display_policy",
        stream::OSD_HARDWARE => "osd_hardware",
        stream::NOTIFICATIONS => "notifications",
        stream::NOTIFICATION_ACTIVE => "notification_active",
        stream::UPDATES => "updates",
        stream::TIMEZONE => "timezone",
        _ => unreachable!("validated stream"),
    }
}

pub(crate) fn initial_stream_data(stream: &str, snapshot: &BarSnapshot) -> Value {
    match stream {
        stream::ACTIVITY => serde_json::to_value(&snapshot.activity),
        stream::WORKAREA => serde_json::to_value(&snapshot.workarea),
        stream::COMPOSITOR => serde_json::to_value(&snapshot.compositor),
        stream::WORKSPACES => serde_json::to_value(&snapshot.workspaces),
        stream::MEDIA => serde_json::to_value(&snapshot.media),
        stream::AUDIO => serde_json::to_value(&snapshot.audio),
        stream::BRIGHTNESS => serde_json::to_value(&snapshot.brightness),
        stream::BATTERY => serde_json::to_value(&snapshot.battery),
        stream::POWER_PROFILE => serde_json::to_value(&snapshot.power_profile),
        stream::POWER_SLEEP => serde_json::to_value(&snapshot.power_sleep),
        stream::SLEEP_POLICY => serde_json::to_value(&snapshot.sleep_policy),
        stream::DISPLAY_POLICY => serde_json::to_value(&snapshot.display_policy),
        stream::OSD_HARDWARE => serde_json::to_value(&snapshot.osd_hardware),
        stream::NOTIFICATIONS => serde_json::to_value(&snapshot.notifications),
        stream::NOTIFICATION_ACTIVE => serde_json::to_value(&snapshot.notification_active),
        stream::UPDATES => serde_json::to_value(&snapshot.updates),
        stream::TIMEZONE => serde_json::to_value(&snapshot.timezone),
        _ => return Value::Null,
    }
    .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_envelopes_preserve_v1_nesting_identity_and_correlation() {
        let mut state = BarSnapshot::default();
        state.compositor.available = true;
        state.compositor.animations_enabled = Some(false);
        let reply = snapshot(&state);
        assert_eq!(reply, api::success(serde_json::json!({"snapshot": state})));
        for stream in super::super::STREAMS {
            let subscribed = Event::subscribed(stream, "sub", &state);
            assert_eq!(subscribed.stream(), subscribed.value()["stream"]);
            assert_eq!(subscribed.value()["protocol"], NAME);
            assert_eq!(subscribed.value()["version"], VERSION);
            assert_eq!(subscribed.value()["subscription_id"], "sub");
            assert_eq!(subscribed.value()["event"], "subscribed");
            assert!(subscribed.value()["data"].is_object(), "{stream}");
            let changed = Event::changed(
                "sub",
                DomainEvent {
                    stream: (*stream).into(),
                    data: subscribed.value()["data"].clone(),
                },
            );
            assert_eq!(changed.value()["event"], "changed");
            assert_eq!(changed.value()["data"], subscribed.value()["data"]);
            assert_eq!(changed.stream(), changed.value()["stream"]);
            let gap = Event::lagged(stream, "sub", 4).into_value();
            assert_eq!(gap["event"], "lagged");
            assert_eq!(gap["data"], serde_json::json!({"missed": 4}));
            assert!(gap.get("missed").is_none());
        }
        let compositor = Event::subscribed(stream::COMPOSITOR, "sub", &state).into_value();
        assert_eq!(compositor["data"]["animations_enabled"], false);
        assert!(compositor.get("animations_enabled").is_none());
    }
}
