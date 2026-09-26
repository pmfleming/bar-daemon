use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

const SETTLE: Duration = Duration::from_secs(5);
const MAX_OBSERVATION_GAP: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub(crate) struct Output {
    pub id: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub width: u32,
    pub height: u32,
    pub disabled: bool,
    #[serde(default)]
    pub focused: bool,
    pub scale: f64,
    #[serde(rename = "refreshRate")]
    pub refresh_rate: f64,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub transform: u8,
    #[serde(default, rename = "availableModes")]
    pub available_modes: Vec<String>,
    #[serde(default, rename = "mirrorOf", deserialize_with = "mirror_reference")]
    pub mirror_of: String,
    // DPMS intentionally does not participate in eligibility: idle blanking
    // must not turn another display on.
}

fn mirror_reference<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Reference {
        Name(String),
        Id(i64),
        None(()),
    }
    let value = match Reference::deserialize(deserializer)? {
        Reference::Name(value) => value,
        Reference::Id(value) => value.to_string(),
        Reference::None(()) => String::new(),
    };
    Ok(if value == "none" || value == "-1" {
        String::new()
    } else {
        value
    })
}

impl Output {
    pub fn internal(&self) -> bool {
        ["eDP-", "LVDS-", "DSI-"]
            .iter()
            .any(|prefix| self.name.starts_with(prefix))
    }
    fn external(&self) -> bool {
        ["DP-", "HDMI-A-"]
            .iter()
            .any(|prefix| self.name.starts_with(prefix))
    }
    pub(super) fn active(&self) -> bool {
        !self.disabled && self.width > 0 && self.height > 0
    }
    pub(super) fn mirrored(&self) -> bool {
        !self.mirror_of.is_empty() && self.mirror_of != "none" && self.mirror_of != "-1"
    }
    pub(super) fn mirror_source<'a>(&self, outputs: &'a [Output]) -> Option<&'a Output> {
        self.mirrored()
            .then(|| {
                outputs
                    .iter()
                    .find(|o| o.name == self.mirror_of || o.id.to_string() == self.mirror_of)
            })
            .flatten()
    }
    // A mirror cannot keep a desktop alive after its source is disabled.
    pub(super) fn usable(&self) -> bool {
        self.active() && !self.mirrored()
    }
    pub fn command(&self, disable: bool) -> Result<String> {
        // Only compositor-reported internal connectors can be mutated. No
        // untrusted connector name is interpolated into executable Lua.
        if !self.internal()
            || !self
                .name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            bail!("invalid internal connector name");
        }
        if disable {
            Ok(format!(
                "eval hl.monitor({{ output = \"{}\", disabled = true }})",
                self.name
            ))
        } else {
            if !self.scale.is_finite() || !(0.25..=8.0).contains(&self.scale) {
                bail!("invalid internal display scale");
            }
            Ok(format!(
                "eval hl.monitor({{ output = \"{}\", mode = \"preferred\", position = \"auto\", scale = {}, mirror = \"\" }})",
                self.name, self.scale
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Signature(String, i64, u32, u32, u64, u64);

pub(super) fn external_signature(outputs: &[Output]) -> Vec<Signature> {
    let mut keys: Vec<_> = outputs
        .iter()
        .filter(|o| o.external() && o.usable())
        .map(|o| {
            Signature(
                o.name.clone(),
                o.id,
                o.width,
                o.height,
                o.scale.to_bits(),
                o.refresh_rate.to_bits(),
            )
        })
        .collect();
    keys.sort();
    keys
}

pub(super) struct Plan {
    pub disable_internal: bool,
    pub targets: Vec<Output>,
    pub status: &'static str,
    pub external: Vec<Signature>,
}

#[derive(Default)]
pub(super) struct Planner {
    candidate: Option<(Vec<Signature>, Instant)>,
    last_observation: Option<Instant>,
}

impl Planner {
    pub fn reset(&mut self) {
        self.candidate = None;
        self.last_observation = None;
    }

    pub fn plan(&mut self, prefer_external: bool, outputs: &[Output], now: Instant) -> Plan {
        if self
            .last_observation
            .is_some_and(|previous| now.duration_since(previous) > MAX_OBSERVATION_GAP)
        {
            self.reset();
        }
        self.last_observation = Some(now);
        let external = external_signature(outputs);
        let (disable_internal, status) = if !prefer_external {
            self.candidate = None;
            (false, "all-displays")
        } else if external.is_empty() {
            self.candidate = None;
            (false, "internal")
        } else {
            let (previous, since) = self
                .candidate
                .get_or_insert_with(|| (external.clone(), now));
            if *previous != external {
                previous.clone_from(&external);
                *since = now;
            }
            if now.duration_since(*since) >= SETTLE {
                (true, "external")
            } else {
                (false, "settling")
            }
        };
        let targets = outputs
            .iter()
            .filter(|o| {
                o.internal()
                    && if disable_internal {
                        o.usable()
                            && !outputs.iter().any(|m| {
                                m.active()
                                    && m.mirror_source(outputs)
                                        .is_some_and(|source| source.name == o.name)
                            })
                    } else {
                        !o.active() || (o.mirrored() && !outputs.iter().any(Output::usable))
                    }
            })
            .cloned()
            .collect();
        Plan {
            disable_internal,
            targets,
            status,
            external,
        }
    }
}
