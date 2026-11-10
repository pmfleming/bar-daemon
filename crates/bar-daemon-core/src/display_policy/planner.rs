use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use shelllist_hyprland::monitor::{self, Mode, Position, Rule};

const SETTLE: Duration = Duration::from_secs(5);
// Permit the 30-second healthy fallback interval; a stalled observer still
// invalidates settling evidence. Topology changes and failed reads reset sooner.
const MAX_OBSERVATION_GAP: Duration = Duration::from_secs(65);

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub(crate) struct Output {
    pub id: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub make: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub serial: String,
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
    #[serde(default)]
    pub vrr: Option<bool>,
    #[serde(default, rename = "currentFormat")]
    pub current_format: Option<String>,
    #[serde(default, rename = "colorManagementPreset")]
    pub color_management_preset: Option<String>,
    #[serde(default, rename = "sdrBrightness")]
    pub sdr_brightness: Option<f64>,
    #[serde(default, rename = "sdrSaturation")]
    pub sdr_saturation: Option<f64>,
    #[serde(default, rename = "hardwareDetails")]
    pub hardware_details: super::layout::advanced::Hardware,
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
    pub(super) fn has_active_mirrors(&self, outputs: &[Output]) -> bool {
        outputs.iter().any(|output| {
            output.active()
                && output
                    .mirror_source(outputs)
                    .is_some_and(|source| source.name == self.name)
        })
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
    pub fn command(&self, disable: bool) -> Result<shelllist_hyprland::Lua> {
        // Automatic docking may mutate only compositor-reported internal
        // connectors. Syntax encoding belongs to the shared Hyprland adapter.
        if !self.internal()
            || !self
                .name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            bail!("invalid internal connector name");
        }
        let rule = if disable {
            Rule::Disable
        } else {
            if !self.scale.is_finite() || !(0.25..=8.0).contains(&self.scale) {
                bail!("invalid internal display scale");
            }
            Rule::Enable {
                mode: Mode::Preferred,
                position: Position::Auto,
                scale: self.scale,
                transform: None,
                mirror: "",
            }
        };
        monitor::command(&self.name, rule)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Signature(String, i64, u32, u32, u64, u64);

fn external_signature(outputs: &[Output]) -> Vec<Signature> {
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

pub(super) struct Plan<'a> {
    pub disable_internal: bool,
    pub targets: Vec<&'a Output>,
    pub status: &'static str,
    external: Vec<Signature>,
    internal: Vec<&'a Output>,
}

impl Plan<'_> {
    pub fn observed(&self, current: &[Output]) -> bool {
        external_signature(current) == self.external
            && current.iter().filter(|o| o.internal()).count() == self.internal.len()
            && self.internal.iter().all(|previous| {
                current.iter().any(|o| {
                    o.name == previous.name
                        && o.id == previous.id
                        && if self.disable_internal {
                            o.disabled
                        } else {
                            o.active()
                        }
                })
            })
            && (self.internal.is_empty() || current.iter().any(Output::usable))
    }

    pub fn validate_disable(&self, target: &Output, current: &[Output]) -> Result<()> {
        if external_signature(current) != self.external
            || !current
                .iter()
                .any(|o| o.name == target.name && o.id == target.id)
            || target.has_active_mirrors(current)
        {
            bail!(
                "display topology changed before disabling the laptop screen; keeping the fallback"
            );
        }
        Ok(())
    }
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

    pub fn plan<'a>(
        &mut self,
        prefer_external: bool,
        outputs: &'a [Output],
        now: Instant,
    ) -> Plan<'a> {
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
                        o.usable() && !o.has_active_mirrors(outputs)
                    } else {
                        !o.active() || (o.mirrored() && !outputs.iter().any(Output::usable))
                    }
            })
            .collect();
        Plan {
            disable_internal,
            targets,
            status,
            external,
            internal: outputs.iter().filter(|o| o.internal()).collect(),
        }
    }
}
