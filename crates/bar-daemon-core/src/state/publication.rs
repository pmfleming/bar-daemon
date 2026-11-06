//! Publication identity belongs to the snapshot commit lock, never a writer.
use serde::{Deserialize, Serialize};
pub(crate) const MAX_REVISION: u64 = (1_u64 << 53) - 1;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct Publication {
    pub epoch: String,
    #[schemars(range(min = 1, max = 9007199254740991_u64))]
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_epoch: Option<String>,
}
fn epoch() -> String {
    let mut bytes = [0_u8; 32];
    let mut remaining = bytes.as_mut_slice();
    while !remaining.is_empty() {
        match rustix::rand::getrandom(&mut *remaining, rustix::rand::GetRandomFlags::empty()) {
            Ok(0) => panic!("empty publication entropy"),
            Ok(n) => remaining = &mut remaining[n..],
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => panic!("publication entropy unavailable: {error}"),
        }
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
impl Publication {
    pub(crate) fn new() -> Self {
        Self {
            epoch: epoch(),
            revision: 1,
            previous_epoch: None,
        }
    }
    pub(crate) fn advance(&mut self) {
        if self.revision == MAX_REVISION {
            self.previous_epoch = Some(std::mem::replace(&mut self.epoch, epoch()));
            self.revision = 1;
        } else {
            self.revision += 1;
        }
    }
}

/// Offline fixture identity only; preserve equivalence and lineage throughout
/// each producer-generated fixture, rather than replacing live metadata.
pub(crate) fn normalize(mut value: serde_json::Value) -> serde_json::Value {
    fn walk(
        value: &mut serde_json::Value,
        epochs: &mut std::collections::BTreeMap<String, String>,
    ) {
        match value {
            serde_json::Value::Object(fields) => {
                if let Some(serde_json::Value::Object(state)) = fields.get_mut("_publication") {
                    for key in ["epoch", "previous_epoch"] {
                        if let Some(serde_json::Value::String(epoch)) = state.get_mut(key) {
                            let next = format!("fixture-publication-{}", epochs.len() + 1);
                            *epoch = epochs.entry(epoch.clone()).or_insert(next).clone();
                        }
                    }
                }
                for value in fields.values_mut() {
                    walk(value, epochs);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    walk(value, epochs);
                }
            }
            _ => {}
        }
    }
    walk(&mut value, &mut Default::default());
    value
}
