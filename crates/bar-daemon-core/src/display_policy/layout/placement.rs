//! Resolve a bounded dependency graph in logical pixels before validation/application.
use super::{Layout, mode};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Relative {
    pub reference: String,
    pub side: Side,
    pub alignment: Alignment,
    pub gap: u16,
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Side {
    Left,
    Right,
    Above,
    Below,
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Alignment {
    Start,
    Center,
    End,
}

pub(super) fn resolve(layout: &mut Layout) -> Result<()> {
    ensure!(layout.outputs.len() <= 16, "Too many displays");
    let mut visited = vec![0u8; layout.outputs.len()];
    for index in 0..layout.outputs.len() {
        visit(layout, index, &mut visited)?;
    }
    Ok(())
}
fn size(s: &super::Setting) -> Result<(i32, i32)> {
    s.validate()?;
    let (w, h, _) = mode(&s.mode).context("Invalid relative display mode")?;
    let (w, h) = if s.transform % 2 == 1 { (h, w) } else { (w, h) };
    Ok((
        (f64::from(w) / s.scale).round().max(1.0) as i32,
        (f64::from(h) / s.scale).round().max(1.0) as i32,
    ))
}
fn visit(layout: &mut Layout, index: usize, visited: &mut [u8]) -> Result<()> {
    ensure!(
        visited[index] != 1,
        "Relative display placement contains a cycle"
    );
    if visited[index] == 2 {
        return Ok(());
    }
    visited[index] = 1;
    if let Some(relative) = layout.outputs[index].relative.clone() {
        ensure!(
            relative.gap <= 4096,
            "Display gap exceeds 4096 logical pixels"
        );
        let own = &layout.outputs[index];
        // Disabled displays retain their relationship for the next enablement.
        if own.enabled {
            ensure!(
                own.mirror_of.is_empty(),
                "Mirrors cannot have relative placement"
            );
            let target = layout
                .outputs
                .iter()
                .position(|s| s.name == relative.reference)
                .context("Relative display anchor is missing")?;
            ensure!(
                layout.outputs[target].enabled && layout.outputs[target].mirror_of.is_empty(),
                "Relative anchor must be enabled and independent"
            );
            visit(layout, target, visited)?;
            let (w, h) = size(&layout.outputs[index])?;
            let anchor = &layout.outputs[target];
            let (aw, ah) = size(anchor)?;
            let offset = |extent: i32| match relative.alignment {
                Alignment::Start => 0,
                Alignment::Center => extent / 2,
                Alignment::End => extent,
            };
            let gap = i32::from(relative.gap);
            let (x, y) = match relative.side {
                Side::Left => (anchor.x - w - gap, anchor.y + offset(ah - h)),
                Side::Right => (anchor.x + aw + gap, anchor.y + offset(ah - h)),
                Side::Above => (anchor.x + offset(aw - w), anchor.y - h - gap),
                Side::Below => (anchor.x + offset(aw - w), anchor.y + ah + gap),
            };
            layout.outputs[index].x = x;
            layout.outputs[index].y = y;
            layout.outputs[index].validate()?;
        }
    }
    visited[index] = 2;
    Ok(())
}
