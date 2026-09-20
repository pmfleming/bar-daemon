//! Battery-level profile overrides. Saver/performance use cooperative PPD holds.
//! Balanced has no HoldProfile support: remember the selected profile durably and
//! only change/restore it when no other application has a hold.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use shelllist_daemon_core::XdgRoot;

use crate::{
    battery::levels::BatteryLevel,
    model::{BatteryAutomationState, BatteryState, PowerProfileState},
    paths::{data_file, load_json_or_default, save_json_atomic},
};

use super::{BUS, HOLD_APPLICATION_ID, INTERFACE, PATH, read_raw_state};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Runtime {
    level: BatteryLevel,
    manual_override: bool,
    balanced_restore_profile: Option<String>,
}

pub(super) struct PowerEnvelope {
    connection: Option<zbus::Connection>,
    owner: String,
    state_path: std::path::PathBuf,
    cookie: Option<u32>,
    held_profile: Option<String>,
    level: BatteryLevel,
    runtime: Runtime,
    saved: Runtime,
    loaded: bool,
    pub(super) status: BatteryAutomationState,
}

impl Default for PowerEnvelope {
    fn default() -> Self {
        Self {
            connection: None,
            owner: String::new(),
            state_path: runtime_path(),
            cookie: None,
            held_profile: None,
            level: BatteryLevel::Normal,
            runtime: Runtime::default(),
            saved: Runtime::default(),
            loaded: false,
            status: BatteryAutomationState::default(),
        }
    }
}

impl PowerEnvelope {
    #[cfg(test)]
    pub(super) fn for_test(state_path: std::path::PathBuf) -> Self {
        Self {
            state_path,
            ..Self::default()
        }
    }

    pub(super) async fn attach(&mut self, connection: zbus::Connection) -> Result<()> {
        if !self.loaded {
            self.runtime = load_json_or_default(&self.state_path, "battery profile state").await?;
            self.saved = self.runtime.clone();
            self.level = self.runtime.level;
            self.loaded = true;
        }
        self.connection = Some(connection);
        self.cookie = None;
        self.held_profile = None;
        Ok(())
    }

    pub(super) fn detach(&mut self) {
        self.connection = None;
        self.cookie = None;
        self.held_profile = None;
    }

    fn observe(&mut self, battery: &BatteryState) {
        if battery.available {
            self.level = self.level.observe(battery);
            self.runtime.level = self.level;
        }
        if self.level == BatteryLevel::Normal && battery.available {
            self.runtime.manual_override = false;
        }
        self.status = BatteryAutomationState {
            level: self.level.as_str().into(),
            status: if !battery.available {
                "unavailable"
            } else if self.level == BatteryLevel::Normal {
                "waiting"
            } else if self.runtime.manual_override {
                "paused"
            } else if self.level.action(battery).profile().is_none() {
                "keep-current"
            } else {
                "active"
            }
            .into(),
            profile: self
                .level
                .action(battery)
                .profile()
                .unwrap_or_default()
                .into(),
            error: None,
        };
    }

    async fn persist(&mut self) -> Result<()> {
        if self.runtime != self.saved {
            save_json_atomic(&self.state_path, &self.runtime).await?;
            self.saved = self.runtime.clone();
        }
        Ok(())
    }

    pub(super) async fn reconcile(&mut self, battery: &BatteryState) -> Result<()> {
        self.observe(battery);
        // Don't restore a persisted Balanced override against the initial,
        // unavailable battery snapshot while providers are still starting.
        if !battery.available {
            return Ok(());
        }
        self.persist().await?;
        let Some(connection) = self.connection.clone() else {
            self.status.status = "unavailable".into();
            return Ok(());
        };
        self.observe_external_selection(&connection, battery)
            .await?;
        let desired = if self.runtime.manual_override {
            None
        } else {
            self.level.action(battery).profile()
        };
        if self.held_profile.as_deref() != desired {
            self.release(&connection).await?;
        }
        if desired != Some("balanced") && !self.restore_balanced(&connection).await? {
            self.status.status = "blocked".into();
            return Ok(());
        }
        if let Some(profile) = desired {
            self.apply_profile(&connection, profile).await?;
        }
        Ok(())
    }

    async fn observe_external_selection(
        &mut self,
        connection: &zbus::Connection,
        battery: &BatteryState,
    ) -> Result<()> {
        if connection.is_bus() {
            let dbus = zbus::fdo::DBusProxy::new(connection).await?;
            let owner = dbus.get_name_owner(BUS.try_into()?).await?.to_string();
            if self.owner != owner {
                // Service restarts destroy holds; they are not manual overrides.
                self.owner = owner;
                self.cookie = None;
                self.held_profile = None;
            }
        }
        let current = read_raw_state(connection).await?;
        let lost_hold = self.cookie.is_some() && !has_own_hold(&current);
        let replaced_balanced = self.runtime.balanced_restore_profile.is_some()
            && current.profile != "balanced"
            && current.active_holds.is_empty();
        if lost_hold {
            self.cookie = None;
            self.held_profile = None;
        }
        if replaced_balanced {
            self.runtime.balanced_restore_profile = None;
        }
        if lost_hold || replaced_balanced {
            // Persist one coherent observation; never reacquire a hold released
            // by a user's external selection during a low-battery episode.
            self.runtime.manual_override = self.level != BatteryLevel::Normal;
            self.persist().await?;
            self.observe(battery);
        }
        Ok(())
    }

    async fn apply_profile(&mut self, connection: &zbus::Connection, profile: &str) -> Result<()> {
        let current = read_raw_state(connection).await?;
        if !current.profiles.iter().any(|item| item.name == profile) {
            self.status.status = "unavailable".into();
            return Ok(());
        }
        if profile == "balanced" {
            if !self.apply_balanced(connection, current).await? {
                self.status.status = "blocked".into();
                return Ok(());
            }
        } else if self.cookie.is_none() {
            let proxy = zbus::Proxy::new(connection, BUS, PATH, INTERFACE).await?;
            let reason = format!("Battery level is {}", self.level.as_str());
            self.cookie = Some(
                proxy
                    .call("HoldProfile", &(profile, reason, HOLD_APPLICATION_ID))
                    .await
                    .context("hold battery-level power profile")?,
            );
            self.held_profile = Some(profile.into());
        }
        if read_raw_state(connection).await?.profile != profile {
            self.status.status = "blocked".into();
        }
        Ok(())
    }

    async fn apply_balanced(
        &mut self,
        connection: &zbus::Connection,
        current: PowerProfileState,
    ) -> Result<bool> {
        if !current.active_holds.is_empty() {
            return Ok(false);
        }
        if self.runtime.balanced_restore_profile.is_some() || current.profile == "balanced" {
            return Ok(true);
        }
        self.runtime.balanced_restore_profile = Some(current.profile);
        if let Err(error) = self.persist().await {
            self.runtime.balanced_restore_profile = None;
            return Err(error);
        }
        let proxy = zbus::Proxy::new(connection, BUS, PATH, INTERFACE).await?;
        if let Err(error) = proxy.set_property("ActiveProfile", &"balanced").await {
            self.runtime.balanced_restore_profile = None;
            self.persist().await?;
            return Err(error).context("apply balanced battery profile");
        }
        Ok(true)
    }

    async fn release(&mut self, connection: &zbus::Connection) -> Result<()> {
        if let Some(cookie) = self.cookie {
            // It may already have been released by an external manual change.
            if has_own_hold(&read_raw_state(connection).await?) {
                let proxy = zbus::Proxy::new(connection, BUS, PATH, INTERFACE).await?;
                let _: () = proxy
                    .call("ReleaseProfile", &(cookie,))
                    .await
                    .context("release battery-level power profile")?;
            }
            self.cookie = None;
            self.held_profile = None;
        }
        Ok(())
    }

    async fn restore_balanced(&mut self, connection: &zbus::Connection) -> Result<bool> {
        let Some(previous) = self.runtime.balanced_restore_profile.as_deref() else {
            return Ok(true);
        };
        let current = read_raw_state(connection).await?;
        if !current.active_holds.is_empty() {
            return Ok(false);
        }
        if current.profile == "balanced"
            && current.profiles.iter().any(|item| item.name == previous)
        {
            let proxy = zbus::Proxy::new(connection, BUS, PATH, INTERFACE).await?;
            proxy
                .set_property("ActiveProfile", &previous)
                .await
                .context("restore pre-battery profile")?;
        }
        self.runtime.balanced_restore_profile = None;
        self.persist().await?;
        Ok(true)
    }

    pub(super) async fn select_manual(
        &mut self,
        profile: &str,
        battery: &BatteryState,
        connection: &zbus::Connection,
    ) -> Result<()> {
        anyhow::ensure!(
            self.loaded && self.connection.is_some(),
            "battery profile automation is reconnecting"
        );
        self.observe(battery);
        let was_paused = self.runtime.manual_override;
        // Persist the pause first so a daemon restart cannot undo user intent.
        self.runtime.manual_override = self.level != BatteryLevel::Normal;
        if let Err(error) = self.persist().await {
            self.runtime.manual_override = was_paused;
            return Err(error);
        }
        let selection: Result<()> = async {
            self.release(connection).await?;
            let proxy = zbus::Proxy::new(connection, BUS, PATH, INTERFACE).await?;
            proxy
                .set_property("ActiveProfile", &profile)
                .await
                .context("set active power profile")
        }
        .await;
        if let Err(error) = selection {
            self.runtime.manual_override = was_paused;
            self.persist().await?;
            if let Err(restore_error) = self.reconcile(battery).await {
                anyhow::bail!("{error}; restore battery automation: {restore_error}");
            }
            return Err(error);
        }
        // The new manual selection supersedes the pre-automation profile.
        self.runtime.balanced_restore_profile = None;
        self.persist().await?;
        self.observe(battery);
        Ok(())
    }

    pub(super) async fn resume(&mut self, battery: &BatteryState) -> Result<()> {
        anyhow::ensure!(
            self.loaded && self.connection.is_some(),
            "battery profile automation is reconnecting"
        );
        self.runtime.manual_override = false;
        self.persist().await?;
        self.reconcile(battery).await
    }
}

fn has_own_hold(state: &PowerProfileState) -> bool {
    state
        .active_holds
        .iter()
        .any(|hold| hold.application_id == HOLD_APPLICATION_ID)
}

fn runtime_path() -> std::path::PathBuf {
    data_file(XdgRoot::State, "battery-profile-state.json")
}

#[cfg(test)]
mod tests {
    use super::PowerEnvelope;
    use crate::model::{BatteryProfileAction, BatteryState};

    fn battery(percentage: u8) -> BatteryState {
        BatteryState {
            available: true,
            percentage,
            ..Default::default()
        }
    }

    #[test]
    fn notification_switches_do_not_control_profile_actions() {
        let mut state = battery(10);
        state.policy.notify_warning = false;
        state.policy.notify_critical = false;
        state.policy.warning_profile = BatteryProfileAction::Balanced;
        let mut envelope = PowerEnvelope::default();
        envelope.observe(&state);
        assert_eq!(envelope.status.profile, "power-saver");
        state.percentage = 20;
        envelope.observe(&state);
        assert_eq!(envelope.status.profile, "balanced");
        state.policy.warning_profile = BatteryProfileAction::KeepCurrent;
        envelope.observe(&state);
        assert_eq!(envelope.status.status, "keep-current");
    }
}
