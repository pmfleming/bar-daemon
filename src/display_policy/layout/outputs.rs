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

#[cfg(test)]
mod tests {
    use super::{Output, Setting, current_mode};
    use crate::display_policy::DisplayPolicyState;
    use serde_json::{Value, json};

    fn output() -> Output {
        Output {
            id: 0,
            name: "eDP-1".into(),
            width: 1920,
            height: 1200,
            refresh_rate: 59.94,
            scale: 1.25,
            available_modes: vec!["1920x1200@60.00Hz".into(), "1920x1200@59.940Hz".into()],
            ..Default::default()
        }
    }
    fn wire(outputs: Vec<Output>) -> Value {
        serde_json::to_value(DisplayPolicyState {
            outputs,
            ..Default::default()
        })
        .unwrap()["outputs"]
            .clone()
    }

    #[test]
    fn exact_closest_modes_and_observed_layout_agree() {
        let output = output();
        assert_eq!(current_mode(&output), "1920x1200@59.940Hz");
        assert_eq!(
            Setting::observed(&output, std::slice::from_ref(&output)).mode,
            current_mode(&output)
        );
        let value = wire(vec![output]);
        assert_eq!(value[0]["current_mode"], "1920x1200@59.940Hz");
        assert_eq!(
            value[0]["modes"][1],
            json!({"id":"1920x1200@59.940Hz", "width":1920, "height":1200, "rate":59.94, "size":"1920x1200"})
        );
        assert_eq!(value[0]["internal"], true);
        assert_eq!(value[0]["supported"], true);
    }

    #[test]
    fn disabled_and_missing_modes_do_not_invent_geometry() {
        let mut output = output();
        output.disabled = true;
        output.width = 0;
        output.height = 0;
        output.refresh_rate = 0.0;
        output.available_modes.insert(0, "1920x1200@60;exec".into());
        assert_eq!(current_mode(&output), "1920x1200@60.00Hz");
        assert_eq!(
            wire(vec![output.clone()])[0]["modes"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        output.available_modes.clear();
        assert_eq!(current_mode(&output), "");
        assert_eq!(wire(vec![output])[0]["modes"], json!([]));
    }

    #[test]
    fn unadvertised_observation_and_invalid_modes() {
        let mut output = output();
        output.refresh_rate = 75.0;
        output.available_modes.extend([
            "0x1200@60".into(),
            "1920x1200@1001".into(),
            "16385x1200@60".into(),
        ]);
        let value = wire(vec![output]);
        assert_eq!(value[0]["current_mode"], "1920x1200@75.00");
        assert_eq!(value[0]["modes"].as_array().unwrap().len(), 3);
        assert_eq!(value[0]["modes"][0]["rate"], 75.0);
        for name in ["DP-", "HEADLESS-1", "DP-1;exec", &"DP-".repeat(30)] {
            let mut output = self::output();
            output.name = name.into();
            assert_eq!(wire(vec![output])[0]["supported"], false, "{name}");
        }
    }

    #[test]
    fn mirror_references_are_resolved_against_one_snapshot() {
        let source = output();
        for reference in [json!(0), json!("0"), json!("eDP-1")] {
            let mut raw = serde_json::to_value(&source).unwrap();
            raw["id"] = json!(1);
            raw["name"] = json!("DP-1");
            raw["mirrorOf"] = reference;
            let mirror: Output = serde_json::from_value(raw).unwrap();
            assert_eq!(
                wire(vec![source.clone(), mirror.clone()])[1]["mirror_of"],
                "eDP-1"
            );
            assert_eq!(
                wire(vec![mirror.clone()])[0]["mirror_of"],
                mirror.mirror_of,
                "unresolved references must not appear independent"
            );
            assert_eq!(
                wire(vec![Output {
                    disabled: true,
                    ..mirror
                }])[0]["mirror_of"],
                ""
            );
        }
        for reference in [json!(-1), json!("none"), Value::Null] {
            let mut raw = serde_json::to_value(&source).unwrap();
            raw["mirrorOf"] = reference;
            let output: Output = serde_json::from_value(raw).unwrap();
            assert_eq!(wire(vec![output])[0]["mirror_of"], "");
        }
    }
}
