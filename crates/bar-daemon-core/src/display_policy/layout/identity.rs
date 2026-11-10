//! Resolve saved physical identities to current connectors, with no ambiguous writes.
use super::{Layout, Output};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    pub make: String,
    pub model: String,
    pub serial: String,
    pub description: String,
}
impl Identity {
    pub(super) fn observed(output: &Output) -> Option<Self> {
        (!output.model.is_empty() || !output.serial.is_empty() || !output.description.is_empty())
            .then(|| Self {
                make: output.make.clone(),
                model: output.model.clone(),
                serial: output.serial.clone(),
                description: output.description.clone(),
            })
    }
    pub(crate) fn matches(&self, output: &Output) -> bool {
        if !self.serial.is_empty() {
            self.serial == output.serial && self.make == output.make && self.model == output.model
        } else if !self.model.is_empty() {
            self.make == output.make && self.model == output.model
        } else {
            !self.description.is_empty() && self.description == output.description
        }
    }
}

pub(super) fn resolve(layout: &Layout, outputs: &[Output]) -> Result<Layout> {
    let mut names = BTreeMap::new();
    let mut used = HashSet::new();
    for setting in &layout.outputs {
        let candidates: Vec<_> = outputs
            .iter()
            .filter(|o| {
                setting
                    .identity
                    .as_ref()
                    .map_or(o.name == setting.name, |id| id.matches(o))
            })
            .collect();
        ensure!(
            candidates.len() <= 1,
            "Ambiguous display identity for {}; identify the displays and preview a corrected layout",
            setting.name
        );
        if let Some(output) = candidates.first() {
            ensure!(
                used.insert(&output.name),
                "Two saved displays match {}",
                output.name
            );
            names.insert(setting.name.clone(), output.name.clone());
        }
    }
    let mut resolved = Layout::default();
    for setting in &layout.outputs {
        if let Some(name) = names.get(&setting.name) {
            let mut next = setting.clone();
            next.name.clone_from(name);
            if !next.mirror_of.is_empty() {
                next.mirror_of = names.get(&next.mirror_of).cloned().unwrap_or_default();
            }
            if let Some(relative) = &mut next.relative {
                if let Some(reference) = names.get(&relative.reference) {
                    relative.reference.clone_from(reference);
                } else {
                    next.relative = None;
                }
            }
            resolved.outputs.push(next);
        }
    }
    Ok(resolved)
}
