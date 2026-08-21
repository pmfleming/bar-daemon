//! ThinkPad-focused battery monitoring, policy, and control.
//!
//! The public module boundary stays stable while providers and policy are kept
//! isolated so the UPower compatibility backend can be removed independently.

use std::{env, path::PathBuf, sync::OnceLock, time::Duration};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use tokio::sync::{Mutex, MutexGuard};
use tokio::time::{interval, sleep};
use zvariant::OwnedObjectPath;

use crate::{
    activity::notifications::service::NotificationSink,
    model::{BatteryPolicyState, BatteryState},
    state::StateStore,
};

pub(crate) mod config;
mod model;
mod monitor;
mod policy;
mod sysfs;
mod upower;

pub mod helper;

use policy::{AlertTracker, send_notification};

static BATTERY_EFFECTS: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) const WARNING_PERCENT: u8 = 25;
pub(crate) const CRITICAL_PERCENT: u8 = 12;

pub(crate) async fn lock_effects() -> MutexGuard<'static, ()> {
    BATTERY_EFFECTS.get_or_init(|| Mutex::new(())).lock().await
}

pub async fn refresh_state(store: &StateStore) -> Result<BatteryState> {
    let state = tokio::task::spawn_blocking(|| sysfs::PowerSupplyFs::system().read_state())
        .await
        .context("join battery state refresh")??;
    let state = decorate_from_disk(state).await;
    store.update_battery(state.clone()).await;
    Ok(state)
}

pub async fn monitor(store: StateStore, notifications: NotificationSink) {
    if env::var("BAR_DAEMON_BATTERY_BACKEND").as_deref() == Ok("upower") {
        tracing::warn!(
            "using the compatibility UPower battery backend; ThinkPad protection controls are unavailable"
        );
        monitor_upower_forever(store, notifications).await;
    } else {
        monitor_native(store, notifications).await;
    }
}

async fn monitor_native(store: StateStore, notifications: NotificationSink) {
    let mut alerts = AlertTracker::default();
    let mut native = monitor::NativeMonitor::new(PathBuf::from("/sys/class/power_supply"));
    loop {
        let power_supply = native.power_supply().clone();
        match tokio::task::spawn_blocking(move || power_supply.read_state()).await {
            Ok(Ok(state)) => publish_native(state, &store, &mut alerts, &notifications).await,
            Ok(Err(error)) => publish_error(error, &store).await,
            Err(error) => publish_error(error, &store).await,
        }
        native.changed().await;
    }
}

async fn monitor_upower_forever(store: StateStore, notifications: NotificationSink) {
    let mut alerts = AlertTracker::default();
    loop {
        match zbus::Connection::system().await {
            Ok(connection) => {
                if let Err(error) =
                    monitor_upower(&connection, &store, &mut alerts, &notifications).await
                {
                    tracing::warn!(%error, "UPower monitor disconnected; using sysfs fallback");
                    refresh_native_once(&store, &mut alerts, &notifications).await;
                }
            }
            Err(error) => {
                store
                    .update_battery(BatteryState {
                        error: Some(error.to_string()),
                        ..BatteryState::default()
                    })
                    .await
            }
        }
        sleep(Duration::from_secs(5)).await;
    }
}

async fn monitor_upower(
    connection: &zbus::Connection,
    store: &StateStore,
    alerts: &mut AlertTracker,
    notifications: &NotificationSink,
) -> Result<()> {
    let device_path = upower::display_device_path(connection).await?;
    let root_properties = zbus::Proxy::new(
        connection,
        upower::BUS,
        upower::ROOT_PATH,
        upower::PROPERTIES_INTERFACE,
    )
    .await?;
    let device_properties = zbus::Proxy::new(
        connection,
        upower::BUS,
        device_path.as_str(),
        upower::PROPERTIES_INTERFACE,
    )
    .await?;
    let mut root_changes = root_properties.receive_signal("PropertiesChanged").await?;
    let mut device_changes = device_properties
        .receive_signal("PropertiesChanged")
        .await?;
    let mut fallback = interval(Duration::from_secs(30));
    fallback.tick().await;
    refresh_upower(connection, store, alerts, &device_path, notifications).await;
    loop {
        tokio::select! {
            signal = root_changes.next() => {
                if signal.is_none() { bail!("UPower root property stream ended"); }
                refresh_upower(connection, store, alerts, &device_path, notifications).await;
            }
            signal = device_changes.next() => {
                if signal.is_none() { bail!("UPower device property stream ended"); }
                refresh_upower(connection, store, alerts, &device_path, notifications).await;
            }
            _ = fallback.tick() => refresh_upower(connection, store, alerts, &device_path, notifications).await,
        }
    }
}

async fn refresh_upower(
    connection: &zbus::Connection,
    store: &StateStore,
    alerts: &mut AlertTracker,
    path: &OwnedObjectPath,
    notifications: &NotificationSink,
) {
    match upower::read_state(connection, path).await {
        Ok(state) => {
            let state = decorate_from_disk(state).await;
            publish(state, store, alerts, notifications).await;
        }
        Err(error) => publish_error(error, store).await,
    }
}

async fn refresh_native_once(
    store: &StateStore,
    alerts: &mut AlertTracker,
    notifications: &NotificationSink,
) {
    match tokio::task::spawn_blocking(|| sysfs::PowerSupplyFs::system().read_state()).await {
        Ok(Ok(state)) => publish_native(state, store, alerts, notifications).await,
        Ok(Err(error)) => publish_error(error, store).await,
        Err(error) => publish_error(error, store).await,
    }
}

async fn publish_native(
    state: BatteryState,
    store: &StateStore,
    alerts: &mut AlertTracker,
    notifications: &NotificationSink,
) {
    let state = reconcile_and_decorate(state).await;
    publish(state, store, alerts, notifications).await;
}

async fn reconcile_and_decorate(mut state: BatteryState) -> BatteryState {
    let _guard = lock_effects().await;
    let (mut config, mut runtime, mut policy_error) = load_policy_data().await;
    if runtime.charge_once_active && runtime.charge_once_battery_id.is_empty() {
        runtime.charge_once_battery_id = state.native_path.clone();
        if let Err(error) = config::save_runtime(&runtime).await {
            append_error(&mut policy_error, error.to_string());
        }
    }
    let charge_once_percentage = state
        .devices
        .iter()
        .find(|device| device.id == runtime.charge_once_battery_id)
        .map(|device| device.percentage)
        .unwrap_or(0);
    let should_finish_charge_once = runtime.charge_once_active
        && (!state.plugged
            || charge_once_percentage >= 100
            || runtime.is_expired(config::unix_ms()));
    let mut restored = false;
    let mut config_changed = false;
    let battery_ids = state
        .devices
        .iter()
        .map(|device| device.id.clone())
        .collect::<Vec<_>>();
    for battery_id in battery_ids {
        let device_policy = config.device(&battery_id);
        let target = reconciliation_target(
            &device_policy,
            &runtime,
            should_finish_charge_once,
            &battery_id,
        );
        let Some(device) = state.devices.iter().find(|device| device.id == battery_id) else {
            continue;
        };
        let observed = (
            device.protection.start_percent,
            device.protection.end_percent,
        );
        let accepted = device_policy
            .accepted_reported_start_percent
            .zip(device_policy.accepted_reported_end_percent);
        let already_applied = target.is_some_and(|target| {
            observed == (Some(target.0), Some(target.1))
                || accepted.is_some_and(|accepted| observed == (Some(accepted.0), Some(accepted.1)))
        });
        let can_control = device.protection.supports_start && device.protection.supports_end;
        let applied = match target {
            Some(target) if can_control && !already_applied => {
                match helper::set_thresholds(&battery_id, target.0, target.1).await {
                    Ok(result) => {
                        update_observed_thresholds(
                            &mut state,
                            &battery_id,
                            result.actual_start_percent,
                            result.actual_end_percent,
                        );
                        let policy = config.device_mut(&battery_id);
                        policy.accepted_reported_start_percent = Some(result.actual_start_percent);
                        policy.accepted_reported_end_percent = Some(result.actual_end_percent);
                        config_changed = true;
                        true
                    }
                    Err(error) => {
                        append_error(&mut policy_error, error.to_string());
                        false
                    }
                }
            }
            Some(_) => already_applied,
            None => false,
        };
        if runtime.charge_once_active && battery_id == runtime.charge_once_battery_id {
            restored = applied;
        }
    }

    if config_changed && let Err(error) = config::save_config(&config).await {
        append_error(&mut policy_error, error.to_string());
    }

    if should_finish_charge_once && restored {
        runtime = config::BatteryRuntimeState::default();
        if let Err(error) = config::save_runtime(&runtime).await {
            append_error(&mut policy_error, error.to_string());
        }
    } else if should_finish_charge_once && !restored {
        append_error(
            &mut policy_error,
            format!(
                "cannot restore charge thresholds for {}: the battery is absent or does not expose threshold controls",
                runtime.charge_once_battery_id
            ),
        );
    }
    decorate(state, &config, &runtime, policy_error)
}

fn reconciliation_target(
    device: &config::BatteryDeviceConfig,
    runtime: &config::BatteryRuntimeState,
    should_finish_charge_once: bool,
    battery_id: &str,
) -> Option<(u8, u8)> {
    let charge_once_target = runtime.charge_once_active
        && (runtime.charge_once_battery_id.is_empty()
            || runtime.charge_once_battery_id == battery_id);
    if charge_once_target && !should_finish_charge_once {
        return Some((0, 100));
    }
    if charge_once_target && should_finish_charge_once {
        return runtime
            .restore_start_percent
            .zip(runtime.restore_end_percent)
            .or_else(|| configured_target(device));
    }
    configured_target(device)
}

fn configured_target(device: &config::BatteryDeviceConfig) -> Option<(u8, u8)> {
    device
        .manage_thresholds
        .then_some(if device.protection_enabled {
            (device.protected_start_percent, device.protected_end_percent)
        } else {
            (0, 100)
        })
}

async fn decorate_from_disk(state: BatteryState) -> BatteryState {
    let (config, runtime, error) = load_policy_data().await;
    decorate(state, &config, &runtime, error)
}

async fn load_policy_data() -> (
    config::BatteryConfig,
    config::BatteryRuntimeState,
    Option<String>,
) {
    let (config, config_error) = match config::load_config().await {
        Ok(config) => (config, None),
        Err(error) => (config::BatteryConfig::default(), Some(error.to_string())),
    };
    let (runtime, runtime_error) = match config::load_runtime().await {
        Ok(runtime) => (runtime, None),
        Err(error) => (
            config::BatteryRuntimeState::default(),
            Some(error.to_string()),
        ),
    };
    let mut error = config_error;
    if let Some(runtime_error) = runtime_error {
        append_error(&mut error, runtime_error);
    }
    (config, runtime, error)
}

fn decorate(
    mut state: BatteryState,
    config: &config::BatteryConfig,
    runtime: &config::BatteryRuntimeState,
    policy_error: Option<String>,
) -> BatteryState {
    state.warning = state.available && !state.plugged && state.percentage <= config.warning_percent;
    state.critical =
        state.available && !state.plugged && state.percentage <= config.critical_percent;
    state.policy = BatteryPolicyState {
        warning_percent: config.warning_percent,
        critical_percent: config.critical_percent,
        notify_when_full: config.notify_when_full,
    };
    for device in &mut state.devices {
        let device_config = config.device(&device.id);
        decorate_protection(
            &mut device.protection,
            &device_config,
            runtime,
            &device.id,
            policy_error.clone(),
        );
    }
    state.protection = state
        .devices
        .iter()
        .find(|device| device.id == state.native_path)
        .map(|device| device.protection.clone())
        .unwrap_or_default();
    state.state = semantic_state(&state);
    state
}

fn decorate_protection(
    protection: &mut crate::model::BatteryProtectionState,
    config: &config::BatteryDeviceConfig,
    runtime: &config::BatteryRuntimeState,
    battery_id: &str,
    policy_error: Option<String>,
) {
    protection.managed = config.manage_thresholds;
    protection.desired_start_percent = Some(config.protected_start_percent);
    protection.desired_end_percent = Some(config.protected_end_percent);
    protection.desired_enabled = config.protection_enabled;
    protection.thresholds_verified = if config.protection_enabled {
        (protection.start_percent, protection.end_percent)
            == (
                Some(config.protected_start_percent),
                Some(config.protected_end_percent),
            )
    } else {
        (protection.start_percent, protection.end_percent) == (Some(0), Some(100))
    };
    protection.charge_once_active = runtime.charge_once_active
        && (runtime.charge_once_battery_id.is_empty()
            || runtime.charge_once_battery_id == battery_id);
    if let Some(error) = policy_error {
        protection.error = Some(match protection.error.take() {
            Some(existing) => format!("{existing}; {error}"),
            None => error,
        });
    }
}

fn update_observed_thresholds(state: &mut BatteryState, battery_id: &str, start: u8, end: u8) {
    if let Some(device) = state
        .devices
        .iter_mut()
        .find(|device| device.id == battery_id)
    {
        device.protection.start_percent = Some(start);
        device.protection.end_percent = Some(end);
        device.protection.enabled = start > 0 || end < 100;
    }
}

fn semantic_state(state: &BatteryState) -> String {
    if state.charging {
        return "charging".into();
    }
    if !state.plugged {
        return if state.state == "fully-charged" {
            "fully-charged".into()
        } else {
            "discharging".into()
        };
    }
    match state.protection.charge_behaviour.as_deref() {
        Some("force-discharge") => "calibrating".into(),
        Some("inhibit-charge" | "inhibit-charge-awake") => "charging-inhibited".into(),
        _ if state.state == "fully-charged" => "fully-charged".into(),
        _ if state.state == "pending-charge" => "charge-paused".into(),
        _ if state.protection.enabled => "charge-paused".into(),
        _ => "not-charging".into(),
    }
}

fn append_error(error: &mut Option<String>, addition: String) {
    *error = Some(match error.take() {
        Some(existing) => format!("{existing}; {addition}"),
        None => addition,
    });
}

async fn publish(
    state: BatteryState,
    store: &StateStore,
    alerts: &mut AlertTracker,
    notifications: &NotificationSink,
) {
    if let Some(alert) = alerts.observe(&state, state.policy.notify_when_full) {
        tokio::spawn(send_notification(alert, notifications.clone()));
    }
    store.update_battery(state).await;
}

async fn publish_error(error: impl std::fmt::Display, store: &StateStore) {
    store
        .update_battery(BatteryState {
            error: Some(error.to_string()),
            ..BatteryState::default()
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::{config, reconciliation_target};

    #[test]
    fn unmanaged_thresholds_are_left_alone() {
        let config = config::BatteryDeviceConfig::default();
        assert_eq!(
            reconciliation_target(
                &config,
                &config::BatteryRuntimeState::default(),
                false,
                "BAT0"
            ),
            None
        );
    }

    #[test]
    fn managed_policy_is_reapplied() {
        let config = config::BatteryDeviceConfig {
            manage_thresholds: true,
            protection_enabled: true,
            ..config::BatteryDeviceConfig::default()
        };
        assert_eq!(
            reconciliation_target(
                &config,
                &config::BatteryRuntimeState::default(),
                false,
                "BAT0"
            ),
            Some((75, 80))
        );
    }

    #[test]
    fn charge_once_restores_the_preexisting_thresholds() {
        let config = config::BatteryDeviceConfig::default();
        let runtime = config::BatteryRuntimeState::start_charge_once(1_000, "BAT1".into(), 60, 85);
        assert_eq!(
            reconciliation_target(&config, &runtime, false, "BAT1"),
            Some((0, 100))
        );
        assert_eq!(
            reconciliation_target(&config, &runtime, true, "BAT1"),
            Some((60, 85))
        );
        assert_eq!(
            reconciliation_target(&config, &runtime, false, "BAT0"),
            None
        );
    }
}
