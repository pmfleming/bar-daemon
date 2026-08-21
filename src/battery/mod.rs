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
    let (config, mut runtime, mut policy_error) = load_policy_data().await;
    let should_finish_charge_once = runtime.charge_once_active
        && (!state.plugged || state.percentage >= 100 || runtime.is_expired(config::unix_ms()));
    let target = reconciliation_target(&config, &runtime, should_finish_charge_once);

    let can_control = state.protection.supports_start
        && state.protection.supports_end
        && !state.native_path.is_empty();
    let already_applied = target.is_some_and(|target| {
        (state.protection.start_percent, state.protection.end_percent)
            == (Some(target.0), Some(target.1))
    });
    let restored = match target {
        Some(target) if can_control && !already_applied => {
            match helper::set_thresholds(&state.native_path, target.0, target.1).await {
                Ok((start, end)) => {
                    update_observed_thresholds(&mut state, start, end);
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

    if should_finish_charge_once && restored {
        runtime = config::BatteryRuntimeState::default();
        if let Err(error) = config::save_runtime(&runtime).await {
            append_error(&mut policy_error, error.to_string());
        }
    } else if should_finish_charge_once && (!can_control || target.is_none()) {
        append_error(
            &mut policy_error,
            "cannot restore charge thresholds: this battery does not expose ThinkPad threshold controls"
                .into(),
        );
    }
    decorate(state, &config, &runtime, policy_error)
}

fn reconciliation_target(
    config: &config::BatteryConfig,
    runtime: &config::BatteryRuntimeState,
    should_finish_charge_once: bool,
) -> Option<(u8, u8)> {
    if runtime.charge_once_active && !should_finish_charge_once {
        return Some((0, 100));
    }
    if should_finish_charge_once {
        return runtime
            .restore_start_percent
            .zip(runtime.restore_end_percent)
            .or_else(|| configured_target(config));
    }
    configured_target(config)
}

fn configured_target(config: &config::BatteryConfig) -> Option<(u8, u8)> {
    config
        .manage_thresholds
        .then_some(if config.protection_enabled {
            (config.protected_start_percent, config.protected_end_percent)
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
    decorate_protection(&mut state.protection, config, runtime, policy_error.clone());
    for device in &mut state.devices {
        decorate_protection(
            &mut device.protection,
            config,
            runtime,
            policy_error.clone(),
        );
    }
    state
}

fn decorate_protection(
    protection: &mut crate::model::BatteryProtectionState,
    config: &config::BatteryConfig,
    runtime: &config::BatteryRuntimeState,
    policy_error: Option<String>,
) {
    protection.managed = config.manage_thresholds;
    protection.desired_start_percent = Some(config.protected_start_percent);
    protection.desired_end_percent = Some(config.protected_end_percent);
    protection.desired_enabled = config.protection_enabled;
    protection.charge_once_active = runtime.charge_once_active;
    if let Some(error) = policy_error {
        protection.error = Some(match protection.error.take() {
            Some(existing) => format!("{existing}; {error}"),
            None => error,
        });
    }
}

fn update_observed_thresholds(state: &mut BatteryState, start: u8, end: u8) {
    state.protection.start_percent = Some(start);
    state.protection.end_percent = Some(end);
    state.protection.enabled = start > 0 || end < 100;
    if let Some(device) = state
        .devices
        .iter_mut()
        .find(|device| device.id == state.native_path)
    {
        device.protection.start_percent = Some(start);
        device.protection.end_percent = Some(end);
        device.protection.enabled = start > 0 || end < 100;
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
        let config = config::BatteryConfig::default();
        assert_eq!(
            reconciliation_target(&config, &config::BatteryRuntimeState::default(), false),
            None
        );
    }

    #[test]
    fn managed_policy_is_reapplied() {
        let config = config::BatteryConfig {
            manage_thresholds: true,
            protection_enabled: true,
            ..config::BatteryConfig::default()
        };
        assert_eq!(
            reconciliation_target(&config, &config::BatteryRuntimeState::default(), false),
            Some((75, 80))
        );
    }

    #[test]
    fn charge_once_restores_the_preexisting_thresholds() {
        let config = config::BatteryConfig::default();
        let runtime = config::BatteryRuntimeState::start_charge_once(1_000, 60, 85);
        assert_eq!(
            reconciliation_target(&config, &runtime, false),
            Some((0, 100))
        );
        assert_eq!(
            reconciliation_target(&config, &runtime, true),
            Some((60, 85))
        );
    }
}
