//! Decode frontend drafts at the protocol boundary, never coerce empty or
//! malformed editor buffers to zero. Snapshot presentation metadata is ignored;
//! the daemon's own output catalogue remains authoritative.
use super::{Layout, Setting, mode};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub(super) fn decode<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Setting>, D::Error> {
    let values = Vec::<Value>::deserialize(deserializer)?;
    ensure_size(&values)
        .and_then(|()| values.into_iter().map(setting).collect())
        .map_err(serde::de::Error::custom)
}
fn ensure_size(values: &[Value]) -> Result<()> {
    ensure!(
        (1..=16).contains(&values.len()),
        "Choose between one and sixteen outputs"
    );
    Ok(())
}
fn number(value: &Value) -> Result<f64> {
    let number = if let Some(text) = value.as_str() {
        ensure!(!text.trim().is_empty(), "Display numeric field is empty");
        text.trim().parse().context("Invalid display number")?
    } else {
        value.as_f64().context("Invalid display number")?
    };
    ensure!(number.is_finite(), "Display number must be finite");
    Ok(number)
}
fn integer(value: &Value, min: f64, max: f64) -> Result<i32> {
    let value = number(value)?;
    ensure!(
        value.fract() == 0.0 && (min..=max).contains(&value),
        "Display integer is out of bounds"
    );
    Ok(value as i32)
}
fn setting(value: Value) -> Result<Setting> {
    let object = value.as_object().context("Invalid display draft")?;
    ensure!(
        object.keys().all(|k| [
            "name",
            "mode",
            "x",
            "y",
            "scale",
            "transform",
            "enabled",
            "mirror_of",
            "modes",
            "internal",
            "width",
            "height"
        ]
        .contains(&k.as_str())),
        "Unknown display draft field"
    );
    Ok(Setting {
        name: value["name"].as_str().context("Missing connector")?.into(),
        mode: value["mode"].as_str().context("Missing mode")?.into(),
        x: integer(&value["x"], -32768.0, 32768.0)?,
        y: integer(&value["y"], -32768.0, 32768.0)?,
        scale: number(&value["scale"])?,
        transform: integer(&value["transform"], 0.0, 7.0)? as u8,
        enabled: value["enabled"]
            .as_bool()
            .context("Invalid display enabled value")?,
        mirror_of: match value.get("mirror_of") {
            None => String::new(),
            Some(v) => v.as_str().context("Invalid mirror source")?.into(),
        },
    })
}
fn rect(setting: &Setting) -> Result<(i64, i64, i64, i64)> {
    let (w, h, _) = mode(&setting.mode).context("Invalid display mode")?;
    let (w, h) = if setting.transform % 2 == 1 {
        (h, w)
    } else {
        (w, h)
    };
    Ok((
        setting.x.into(),
        setting.y.into(),
        (f64::from(w) / setting.scale).round().max(1.0) as i64,
        (f64::from(h) / setting.scale).round().max(1.0) as i64,
    ))
}
fn independent(setting: &&Setting) -> bool {
    setting.enabled && setting.mirror_of.is_empty()
}

pub(super) fn validate_arrangement(next: &Layout, previous: &Layout) -> Result<()> {
    let independent: Vec<_> = next.outputs.iter().filter(independent).collect();
    for (index, a) in independent.iter().enumerate() {
        for b in &independent[index + 1..] {
            let (ax, ay, aw, ah) = rect(a)?;
            let (bx, by, bw, bh) = rect(b)?;
            if ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah {
                let unchanged = |current: &Setting| {
                    previous
                        .outputs
                        .iter()
                        .filter(|old| old.enabled && old.mirror_of.is_empty())
                        .find(|old| old.name == current.name)
                        .is_some_and(|old| rect(old).ok() == rect(current).ok())
                };
                ensure!(
                    unchanged(a) && unchanged(b),
                    "{} overlaps {} · arrange displays before previewing",
                    a.name,
                    b.name
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn raw_drafts_preserve_numbers_and_reject_truncation_or_unknown_fields() {
        let base = json!({"name":"eDP-1","mode":"1920x1080@60","x":"-12","y":0,"scale":"1.25","transform":0,"enabled":true,"modes":[{"id":"untrusted"}],"width":999});
        let parsed = setting(base.clone()).unwrap();
        assert_eq!(parsed.x, -12);
        assert_eq!(parsed.scale, 1.25);
        for (key, value) in [
            ("x", json!("")),
            ("x", json!(1.5)),
            ("x", json!(32769)),
            ("scale", json!("Infinity")),
            ("enabled", json!(1)),
            ("mirror_of", json!(null)),
            ("unexpected", json!(true)),
        ] {
            let mut bad = base.clone();
            bad[key] = value;
            assert!(setting(bad).is_err());
        }
    }
    #[test]
    fn only_unchanged_observed_overlaps_are_permitted() {
        let a = setting(json!({"name":"eDP-1","mode":"1920x1080@60","x":0,"y":0,"scale":1,"transform":0,"enabled":true})).unwrap();
        let mut b = a.clone();
        b.name = "DP-1".into();
        let old = Layout {
            outputs: vec![a, b],
        };
        assert!(validate_arrangement(&old, &old).is_ok());
        let mut next = old.clone();
        next.outputs[1].x = 10;
        assert!(validate_arrangement(&next, &old).is_err());
        next.outputs[1].x = 1920;
        assert!(validate_arrangement(&next, &old).is_ok());
        next.outputs[1].x = 0;
        next.outputs[1].mirror_of = "eDP-1".into();
        assert!(validate_arrangement(&next, &old).is_ok());
    }
}
