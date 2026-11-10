//! Named, bounded display profiles. Selection is pure; layout owns all effects.
use super::{
    Output,
    layout::{Layout, Setting},
};
use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};

pub(crate) mod power;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Context {
    pub plugged: Option<bool>,
    pub lid_closed: Option<bool>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    pub items: Vec<Profile>,
    pub automatic: bool,
    pub active: Option<String>,
    pub fallback: Option<String>,
    pub revision: u64,
    /// A failed automatic attempt is durable: retry requires a changed context
    /// or explicit user edit, rather than a restart-triggered mutation loop.
    pub attempted: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profile {
    pub name: String,
    pub layout: Layout,
    pub required: Vec<String>,
    pub priority: i16,
    pub plugged: Option<bool>,
    pub lid_closed: Option<bool>,
}
#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum Request {
    Capture {
        name: String,
    },
    Update {
        name: String,
        renamed: String,
        required: Vec<String>,
        priority: i16,
        plugged: Option<bool>,
        lid_closed: Option<bool>,
    },
    Remove {
        name: String,
    },
    Automatic {
        enabled: bool,
    },
    Fallback {
        name: Option<String>,
    },
    Preview {
        name: String,
    },
}
fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name == name.trim()
        && name.len() <= 80
        && !name.chars().any(char::is_control)
}
impl Catalog {
    pub(crate) fn get(&self, name: &str) -> Result<&Profile> {
        self.items
            .iter()
            .find(|p| p.name == name)
            .context("Display profile no longer exists")
    }
    pub(crate) fn edit(&mut self, request: Request, current: Layout) -> Result<()> {
        match request {
            Request::Capture { name } => {
                ensure!(
                    valid_name(&name),
                    "Use a profile name of 1–80 bytes without control characters"
                );
                ensure!(
                    self.items.len() < 32 && !self.items.iter().any(|p| p.name == name),
                    "Profile already exists or the 32-profile limit was reached"
                );
                self.items.push(Profile {
                    name: name.clone(),
                    required: current.outputs.iter().map(|s| s.name.clone()).collect(),
                    layout: current,
                    priority: 0,
                    plugged: None,
                    lid_closed: None,
                });
                self.active = Some(name);
                self.automatic = false;
            }
            Request::Update {
                name,
                renamed,
                required,
                priority,
                plugged,
                lid_closed,
            } => {
                ensure!(
                    valid_name(&renamed)
                        && (name == renamed || !self.items.iter().any(|p| p.name == renamed)),
                    "Invalid or duplicate profile name"
                );
                let profile = self
                    .items
                    .iter_mut()
                    .find(|p| p.name == name)
                    .context("Display profile no longer exists")?;
                let unique: std::collections::HashSet<_> = required.iter().collect();
                ensure!(
                    unique.len() == required.len()
                        && required.iter().all(|n| profile
                            .layout
                            .outputs
                            .iter()
                            .any(|s| &s.name == n)),
                    "Unknown or duplicate required monitor"
                );
                profile.name.clone_from(&renamed);
                profile.required = required;
                profile.priority = priority;
                profile.plugged = plugged;
                profile.lid_closed = lid_closed;
                if self.active.as_deref() == Some(&name) {
                    self.active = Some(renamed.clone());
                }
                if self.fallback.as_deref() == Some(&name) {
                    self.fallback = Some(renamed);
                }
            }
            Request::Remove { name } => {
                self.get(&name)?;
                self.items.retain(|p| p.name != name);
                if self.active.as_deref() == Some(&name) {
                    self.active = None;
                }
                if self.fallback.as_deref() == Some(&name) {
                    self.fallback = None;
                }
            }
            Request::Automatic { enabled } => self.automatic = enabled,
            Request::Fallback { name } => {
                if let Some(name) = &name {
                    self.get(name)?;
                }
                self.fallback = name;
            }
            Request::Preview { .. } => {
                anyhow::bail!("Profile preview must use the layout transaction")
            }
        }
        self.revision = self
            .revision
            .checked_add(1)
            .context("Profile revision exhausted")?;
        self.attempted = None;
        self.error = None;
        Ok(())
    }
    pub(crate) fn select<'a>(
        &'a self,
        outputs: &[Output],
        context: &Context,
    ) -> Result<Option<&'a Profile>> {
        let mut matches = Vec::new();
        for profile in &self.items {
            if self.fallback.as_deref() != Some(&profile.name)
                && profile.matches(outputs, context)?
            {
                matches.push(profile);
            }
        }
        matches.sort_by_key(|p| {
            std::cmp::Reverse((
                p.priority,
                p.required.len(),
                usize::from(p.plugged.is_some()) + usize::from(p.lid_closed.is_some()),
            ))
        });
        if matches.len() > 1 {
            let rank = |p: &Profile| {
                (
                    p.priority,
                    p.required.len(),
                    usize::from(p.plugged.is_some()) + usize::from(p.lid_closed.is_some()),
                )
            };
            ensure!(
                rank(matches[0]) != rank(matches[1]),
                "Display profiles {} and {} have equal priority and specificity",
                matches[0].name,
                matches[1].name
            );
        }
        if let Some(profile) = matches.first() {
            return Ok(Some(profile));
        }
        Ok(self
            .fallback
            .as_deref()
            .and_then(|name| self.items.iter().find(|p| p.name == name)))
    }
}
impl Profile {
    pub(crate) fn binding(&self, output: &Output) -> Option<&str> {
        self.layout
            .outputs
            .iter()
            .find(|s| Self::bound(s, output))
            .map(|s| s.name.as_str())
    }
    fn bound(setting: &Setting, output: &Output) -> bool {
        setting
            .identity
            .as_ref()
            .map_or(setting.name == output.name, |id| id.matches(output))
    }
    fn matches(&self, outputs: &[Output], context: &Context) -> Result<bool> {
        if self.plugged.is_some() && self.plugged != context.plugged
            || self.lid_closed.is_some() && self.lid_closed != context.lid_closed
        {
            return Ok(false);
        }
        for name in &self.required {
            let setting = self
                .layout
                .outputs
                .iter()
                .find(|s| &s.name == name)
                .context("Invalid required display in saved profile")?;
            let count = outputs.iter().filter(|o| Self::bound(setting, o)).count();
            ensure!(
                count <= 1,
                "Ambiguous required monitor in profile {}",
                self.name
            );
            if count == 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub(crate) fn resolve(&self, outputs: &[Output]) -> Result<Layout> {
        let mut resolved = self.layout.connected(outputs)?;
        // Unknown connected displays retain their observed configuration.
        for setting in Layout::observed(outputs).outputs {
            if !resolved.outputs.iter().any(|s| s.name == setting.name) {
                resolved.outputs.push(setting);
            }
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_requires_known_conditions_and_rejects_equal_rank() {
        let mut catalog = Catalog::default();
        catalog
            .edit(
                Request::Capture {
                    name: "Battery".into(),
                },
                Layout::default(),
            )
            .unwrap();
        catalog.items[0].plugged = Some(false);
        assert!(catalog.select(&[], &Context::default()).unwrap().is_none());
        let battery = Context {
            plugged: Some(false),
            lid_closed: None,
        };
        assert_eq!(
            catalog.select(&[], &battery).unwrap().unwrap().name,
            "Battery"
        );
        let mut duplicate = catalog.items[0].clone();
        duplicate.name = "Conflict".into();
        catalog.items.push(duplicate);
        assert!(catalog.select(&[], &battery).is_err());
        catalog.items[1].priority = 1;
        assert_eq!(
            catalog.select(&[], &battery).unwrap().unwrap().name,
            "Conflict"
        );
        catalog.fallback = Some("Conflict".into());
        assert_eq!(
            catalog
                .select(&[], &Context::default())
                .unwrap()
                .unwrap()
                .name,
            "Conflict"
        );
    }
}
