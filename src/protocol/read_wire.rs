//! Read-only bar-api v1 envelopes, shared by live dispatch and contract fixtures.
use serde::Serialize;
use serde_json::{Value, to_value};
use shelllist_daemon_core::{
    ApiIdentity, Correlation,
    wire::{Event as Envelope, Gap},
};

use super::{NAME, VERSION, stream};
use crate::{model::BarSnapshot, state::DomainEvent};

pub(crate) fn snapshot(snapshot: &BarSnapshot) -> Value {
    let projection: serde_json::Map<String, Value> = STREAM_SPECS
        .iter()
        .map(|spec| (spec.snapshot_key.into(), spec.project(snapshot)))
        .collect();
    super::success(serde_json::json!({"snapshot": projection}))
}

/// Add current cached projections without stamping/replacing effect receipts.
pub(crate) fn reconcile_reply(mut response: Value, snapshot: &BarSnapshot) -> Value {
    if response["ok"] == true {
        let projection: serde_json::Map<String, Value> = STREAM_SPECS
            .iter()
            .filter(|spec| response["data"].get(spec.snapshot_key).is_some())
            .map(|spec| (spec.snapshot_key.into(), spec.project(snapshot)))
            .collect();
        if !projection.is_empty() {
            response["data"]["snapshot"] = Value::Object(projection);
        }
    }
    response
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
            Self::Lagged => "gap",
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
        let value = Envelope::new(
            ApiIdentity::new(NAME, VERSION as u32),
            &stream,
            kind.as_str(),
            Correlation::Subscription(subscription_id),
            data,
        )
        .into_value()
        .expect("serializable bar event");
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
            Gap::lagged(missed),
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

pub(crate) struct StreamSpec {
    pub name: &'static str,
    pub snapshot_key: &'static str,
    read: fn(&BarSnapshot) -> serde_json::Result<Value>,
}
impl StreamSpec {
    pub fn project(&self, snapshot: &BarSnapshot) -> Value {
        let mut value = (self.read)(snapshot).expect("serializable domain projection");
        if let Some(publication) = snapshot.publications.get(self.name) {
            value["_publication"] = to_value(publication).expect("serializable publication");
        }
        value
    }
}
const fn spec(
    name: &'static str,
    snapshot_key: &'static str,
    read: fn(&BarSnapshot) -> serde_json::Result<Value>,
) -> StreamSpec {
    StreamSpec {
        name,
        snapshot_key,
        read,
    }
}

// Authoritative order, snapshot key, and typed field projection for each stream.
pub(crate) const STREAM_SPECS: &[StreamSpec] = &[
    spec(stream::THEME, "theme", |s| to_value(&s.theme)),
    spec(stream::ACTIVITY, "activity", |s| to_value(&s.activity)),
    spec(stream::WORKSPACES, "workspaces", |s| {
        to_value(&s.workspaces)
    }),
    spec(stream::WORKAREA, "workarea", |s| to_value(&s.workarea)),
    spec(stream::COMPOSITOR, "compositor", |s| {
        to_value(&s.compositor)
    }),
    spec(stream::MEDIA, "media", |s| to_value(&s.media)),
    spec(stream::AUDIO, "audio", |s| to_value(&s.audio)),
    spec(stream::BRIGHTNESS, "brightness", |s| {
        to_value(&s.brightness)
    }),
    spec(stream::BATTERY, "battery", |s| to_value(&s.battery)),
    spec(stream::POWER_PROFILE, "power_profile", |s| {
        to_value(&s.power_profile)
    }),
    spec(stream::POWER_SLEEP, "power_sleep", |s| {
        to_value(&s.power_sleep)
    }),
    spec(stream::SLEEP_POLICY, "sleep_policy", |s| {
        to_value(&s.sleep_policy)
    }),
    spec(stream::DISPLAY_POLICY, "display_policy", |s| {
        to_value(&s.display_policy)
    }),
    spec(stream::OSD_HARDWARE, "osd_hardware", |s| {
        to_value(&s.osd_hardware)
    }),
    spec(stream::NOTIFICATIONS, "notifications", |s| {
        to_value(&s.notifications)
    }),
    spec(stream::NOTIFICATION_ACTIVE, "notification_active", |s| {
        to_value(&s.notification_active)
    }),
    spec(stream::UPDATES, "updates", |s| to_value(&s.updates)),
    spec(stream::TIMEZONE, "timezone", |s| to_value(&s.timezone)),
];

// Preserve the public &[&str] contract without maintaining another registry.
pub(super) const STREAM_NAMES: [&str; STREAM_SPECS.len()] = {
    let mut names = [""; STREAM_SPECS.len()];
    let mut i = 0;
    while i < names.len() {
        names[i] = STREAM_SPECS[i].name;
        i += 1;
    }
    names
};

pub(crate) fn stream_spec(name: &str) -> Option<&'static StreamSpec> {
    STREAM_SPECS.iter().find(|spec| spec.name == name)
}

pub(crate) fn initial_stream_data(stream: &str, snapshot: &BarSnapshot) -> Value {
    stream_spec(stream).map_or(Value::Null, |spec| spec.project(snapshot))
}
