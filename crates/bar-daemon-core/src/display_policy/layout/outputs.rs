//! Additive bar-api display projection. Raw compositor fields remain for older
//! clients; new clients never need to parse modes or resolve mirror IDs.
use super::{Output, Setting, connector, mode};
use serde::{Serialize, Serializer};

#[derive(Debug, Serialize)]
struct Mode {
    id: String,
    width: u32,
    height: u32,
    rate: f64,
    size: String,
}
impl Mode {
    fn parse(id: &str) -> Option<Self> {
        let (width, height, rate) = mode(id)?;
        Some(Self {
            id: id.into(),
            width,
            height,
            rate,
            size: format!("{width}x{height}"),
        })
    }
}

pub(super) fn current_mode(output: &Output) -> String {
    let closest = output
        .available_modes
        .iter()
        .filter_map(|id| {
            let (width, height, rate) = mode(id)?;
            let difference = (rate - output.refresh_rate).abs();
            (width == output.width && height == output.height && difference < 0.1)
                .then_some((id, difference))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((id, _)) = closest {
        return id.clone();
    }
    let observed = Setting::observed_mode(output);
    if mode(&observed).is_some() {
        return observed;
    }
    if output.disabled
        && let Some(id) = output.available_modes.iter().find(|id| mode(id).is_some())
    {
        return id.clone();
    }
    // No invented geometry/mode when the compositor has no usable observation.
    String::new()
}

#[derive(Serialize)]
struct NormalizedOutput<'a> {
    #[serde(flatten)]
    observed: &'a Output,
    supported: bool,
    internal: bool,
    current_mode: String,
    modes: Vec<Mode>,
    mirror_of: &'a str,
}

pub(crate) fn serialize_outputs<S: Serializer>(
    outputs: &[Output],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(outputs.iter().map(|output| {
        let current_mode = current_mode(output);
        let mut modes: Vec<_> = output
            .available_modes
            .iter()
            .filter_map(|id| Mode::parse(id))
            .collect();
        if !modes.iter().any(|mode| mode.id == current_mode)
            && let Some(current) = Mode::parse(&current_mode)
        {
            modes.insert(0, current);
        }
        NormalizedOutput {
            observed: output,
            supported: connector(&output.name),
            internal: output.internal(),
            current_mode,
            modes,
            mirror_of: if output.disabled {
                ""
            } else {
                output
                    .mirror_source(outputs)
                    .map_or(&output.mirror_of, |source| &source.name)
            },
        }
    }))
}
