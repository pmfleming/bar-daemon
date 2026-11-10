//! Explicit per-output settings, with unknown telemetry kept distinct from off.
use super::Output;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Advanced {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vrr: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdr_brightness: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdr_saturation: Option<f64>,
}
impl Advanced {
    pub(super) fn wire(&self) -> shelllist_hyprland::monitor::Advanced<'_> {
        shelllist_hyprland::monitor::Advanced {
            vrr: self.vrr,
            bit_depth: self.bit_depth,
            color: self.color.as_deref(),
            sdr_brightness: self.sdr_brightness,
            sdr_saturation: self.sdr_saturation,
        }
    }
    pub(crate) fn observed(o: &Output) -> Self {
        let color = o
            .color_management_preset
            .as_deref()
            .filter(|v| {
                shelllist_hyprland::monitor::Advanced {
                    color: Some(v),
                    ..Default::default()
                }
                .validate()
                .is_ok()
            })
            .map(str::to_owned);
        // DRM format names describe channel depth; never infer support for
        // another format from resolution, manufacturer or a guessed EDID.
        let bit_depth = match o.current_format.as_deref() {
            Some("XRGB8888" | "ARGB8888" | "XBGR8888" | "ABGR8888") => Some(8),
            Some("XRGB2101010" | "ARGB2101010" | "XBGR2101010" | "ABGR2101010") => Some(10),
            _ => None,
        };
        Self {
            vrr: o.vrr,
            bit_depth,
            color,
            sdr_brightness: o
                .sdr_brightness
                .filter(|v| v.is_finite() && (0.1..=3.0).contains(v)),
            sdr_saturation: o
                .sdr_saturation
                .filter(|v| v.is_finite() && (0.0..=2.0).contains(v)),
        }
    }
    pub(super) fn validate_for(&self, output: &Output) -> Result<()> {
        self.wire().validate()?;
        let actual = Self::observed(output);
        ensure!(
            self.vrr.is_none() || actual.vrr.is_some(),
            "VRR readback unavailable"
        );
        ensure!(
            self.bit_depth.is_none() || actual.bit_depth.is_some(),
            "Bit-depth readback unavailable"
        );
        ensure!(
            self.color.is_none() || actual.color.is_some(),
            "Color readback unavailable"
        );
        ensure!(
            self.sdr_brightness.is_none() || actual.sdr_brightness.is_some(),
            "SDR brightness readback unavailable"
        );
        ensure!(
            self.sdr_saturation.is_none() || actual.sdr_saturation.is_some(),
            "SDR saturation readback unavailable"
        );
        let hardware = &output.hardware_details;
        ensure!(
            self.vrr != Some(true)
                || actual.vrr == Some(true)
                || hardware.vrr_capable == Some(true),
            "Display does not report VRR support"
        );
        if self.color != actual.color {
            match self.color.as_deref() {
                Some("hdr" | "hdredid") => ensure!(
                    hardware.hdr == Some(true),
                    "Display does not report HDR support"
                ),
                Some("wide") => ensure!(
                    hardware.bt2020 == Some(true),
                    "Display does not report wide-color support"
                ),
                Some("edid") => ensure!(
                    hardware.chroma == Some(true),
                    "Display does not report EDID primaries"
                ),
                _ => {}
            }
        }
        Ok(())
    }
    pub(super) fn matches(&self, output: &Output) -> bool {
        let actual = Self::observed(output);
        let close = |want: Option<f64>, got: Option<f64>| {
            want.is_none_or(|v| got.is_some_and(|a| (v - a).abs() < 0.001))
        };
        self.vrr.is_none_or(|v| actual.vrr == Some(v))
            && self.bit_depth.is_none_or(|v| actual.bit_depth == Some(v))
            && self
                .color
                .as_ref()
                .is_none_or(|v| actual.color.as_ref() == Some(v))
            && close(self.sdr_brightness, actual.sdr_brightness)
            && close(self.sdr_saturation, actual.sdr_saturation)
    }
    pub(super) fn restrict_to(&mut self, requested: &Self) {
        if requested.vrr.is_none() {
            self.vrr = None;
        }
        if requested.bit_depth.is_none() {
            self.bit_depth = None;
        }
        if requested.color.is_none() {
            self.color = None;
        }
        if requested.sdr_brightness.is_none() {
            self.sdr_brightness = None;
        }
        if requested.sdr_saturation.is_none() {
            self.sdr_saturation = None;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Hardware {
    pub hdr: Option<bool>,
    pub bt2020: Option<bool>,
    pub chroma: Option<bool>,
    #[serde(rename = "vrrCapable")]
    pub vrr_capable: Option<bool>,
}

#[derive(Serialize)]
pub(crate) struct Options {
    vrr: Vec<bool>,
    bit_depth: Vec<u8>,
    color: Vec<&'static str>,
    sdr_brightness: bool,
    sdr_saturation: bool,
}
pub(crate) fn options(output: &Output) -> Options {
    let actual = Advanced::observed(output);
    Options {
        vrr: [false, true]
            .into_iter()
            .filter(|v| {
                Advanced {
                    vrr: Some(*v),
                    ..Default::default()
                }
                .validate_for(output)
                .is_ok()
            })
            .collect(),
        bit_depth: [8, 10]
            .into_iter()
            .filter(|v| {
                Advanced {
                    bit_depth: Some(*v),
                    ..Default::default()
                }
                .validate_for(output)
                .is_ok()
            })
            .collect(),
        color: [
            "srgb", "dcip3", "dp3", "adobe", "wide", "edid", "hdr", "hdredid",
        ]
        .into_iter()
        .filter(|v| {
            Advanced {
                color: Some((*v).into()),
                ..Default::default()
            }
            .validate_for(output)
            .is_ok()
        })
        .collect(),
        sdr_brightness: actual.sdr_brightness.is_some(),
        sdr_saturation: actual.sdr_saturation.is_some(),
    }
}
