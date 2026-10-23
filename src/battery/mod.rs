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
    model::{BatteryDeviceState, BatteryOperationState, BatteryState},
    state::StateStore,
};

pub(crate) mod authorization;
pub(crate) mod config;
pub(crate) mod derived;
mod history;
pub(crate) mod history_chart;
pub(crate) mod levels;
mod model;
mod monitor;
mod policy;
mod sysfs;
mod upower;

pub(crate) mod helper;

use policy::{AlertTracker, send_notification};

static BATTERY_EFFECTS: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) const WARNING_PERCENT: u8 = 25;
pub(crate) const CRITICAL_PERCENT: u8 = 12;

pub(crate) async fn lock_effects() -> MutexGuard<'static, ()> {
    BATTERY_EFFECTS.get_or_init(|| Mutex::new(())).lock().await
}

pub(crate) async fn refresh_state(store: &StateStore) -> Result<BatteryState> {
    let state = tokio::task::spawn_blocking(|| sysfs::PowerSupplyFs::system().read_state())
        .await
        .context("join battery state refresh")??;
    let state = history::attach_summary(decorate_from_disk(state).await);
    store.update_battery(state.clone()).await;
    Ok(state)
}

pub(crate) fn history_snapshot() -> crate::model::BatteryHistoryState {
    history::snapshot()
}

pub(crate) async fn monitor(store: StateStore, notifications: NotificationSink) {
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
    migrate_charge_once_battery(&state, &mut runtime, &mut policy_error).await;
    if !runtime.authorization_uncertain_batteries.is_empty() {
        append_error(&mut policy_error, "Battery write outcome unknown; automatic writes for the affected battery paused. Inspect current state before another action.".into());
    }
    if reconcile_runtime_operation(&mut state, &mut runtime, &mut policy_error).await {
        save_runtime(&runtime, &mut policy_error).await;
    }
    let finish_charge_once = charge_once_is_due(&state, &runtime);
    let (restored, config_changed) = reconcile_thresholds(
        &mut state,
        &mut config,
        &runtime,
        finish_charge_once,
        &mut policy_error,
    )
    .await;
    if config_changed && let Err(error) = config::save_config(&config).await {
        append_error(&mut policy_error, error.to_string());
    }
    finish_charge_once_if_due(
        finish_charge_once,
        restored,
        &mut runtime,
        &mut policy_error,
    )
    .await;
    decorate(state, &config, &runtime, policy_error)
}

async fn migrate_charge_once_battery(
    state: &BatteryState,
    runtime: &mut config::BatteryRuntimeState,
    policy_error: &mut Option<String>,
) {
    if runtime.charge_once_active && runtime.charge_once_battery_id.is_empty() {
        runtime
            .charge_once_battery_id
            .clone_from(&state.native_path);
        save_runtime(runtime, policy_error).await;
    }
}

fn charge_once_is_due(state: &BatteryState, runtime: &config::BatteryRuntimeState) -> bool {
    let percentage = state
        .devices
        .iter()
        .find(|device| device.id == runtime.charge_once_battery_id)
        .map(|device| device.percentage)
        .unwrap_or(0);
    runtime.charge_once_active
        && (!state.plugged || percentage >= 100 || runtime.is_expired(crate::time::unix_ms()))
}

async fn finish_charge_once_if_due(
    due: bool,
    restored: bool,
    runtime: &mut config::BatteryRuntimeState,
    policy_error: &mut Option<String>,
) {
    if !due {
        return;
    }
    if !restored {
        append_error(
            policy_error,
            format!(
                "cannot verify restored charge thresholds for {}: the battery is absent, unsupported, or reported different values",
                runtime.charge_once_battery_id
            ),
        );
        return;
    }
    let mut uncertain = std::mem::take(&mut runtime.authorization_uncertain_batteries);
    uncertain.retain(|id| id != &runtime.charge_once_battery_id);
    *runtime = config::BatteryRuntimeState::default();
    runtime.authorization_uncertain_batteries = uncertain;
    save_runtime(runtime, policy_error).await;
}

async fn save_runtime(runtime: &config::BatteryRuntimeState, policy_error: &mut Option<String>) {
    if let Err(error) = config::save_runtime(runtime).await {
        append_error(policy_error, error.to_string());
    }
}

async fn reconcile_thresholds(
    state: &mut BatteryState,
    config: &mut config::BatteryConfig,
    runtime: &config::BatteryRuntimeState,
    finish_charge_once: bool,
    policy_error: &mut Option<String>,
) -> (bool, bool) {
    let mut restored = false;
    let mut config_changed = false;
    for device in &mut state.devices {
        let intent = threshold_intent(device, config, runtime, finish_charge_once);
        let charge_once_battery =
            runtime.charge_once_active && device.id == runtime.charge_once_battery_id;
        let (applied, changed) = apply_threshold(device, config, intent, policy_error).await;
        config_changed |= changed;
        if charge_once_battery {
            restored = applied;
        }
    }
    (restored, config_changed)
}

struct ThresholdIntent {
    target: Option<(u8, u8)>,
    observed: (Option<u8>, Option<u8>),
    accepted: Option<(u8, u8)>,
    temporary: bool,
    restoration: bool,
    policy: bool,
    can_control: bool,
}

fn threshold_intent(
    device: &BatteryDeviceState,
    config: &config::BatteryConfig,
    runtime: &config::BatteryRuntimeState,
    finish_charge_once: bool,
) -> ThresholdIntent {
    let battery_id = device.id.as_str();
    let device_policy = config.device(battery_id);
    let charge_once = runtime.charge_once_targets(battery_id);
    let temporary = (runtime.operation == config::OperationKind::Calibration
        && runtime.operation_battery_id == battery_id)
        || (charge_once && !finish_charge_once);
    let restoration = charge_once && finish_charge_once;
    ThresholdIntent {
        target: reconciliation_target(&device_policy, runtime, finish_charge_once, battery_id),
        observed: (
            device.protection.start_percent,
            device.protection.end_percent,
        ),
        accepted: device_policy
            .accepted_reported_start_percent
            .zip(device_policy.accepted_reported_end_percent),
        temporary,
        restoration,
        policy: !temporary && !restoration,
        can_control: device.protection.supports_start && device.protection.supports_end,
    }
}

async fn apply_threshold(
    device: &mut BatteryDeviceState,
    config: &mut config::BatteryConfig,
    intent: ThresholdIntent,
    policy_error: &mut Option<String>,
) -> (bool, bool) {
    let Some(target) = intent.target else {
        return (false, false);
    };
    let already_applied = target_is_applied(
        intent.observed,
        target,
        intent.accepted,
        intent.temporary,
        intent.policy,
    );
    if already_applied || !intent.can_control {
        return (already_applied, false);
    }
    let result = match helper::set_thresholds(&device.id, target.0, target.1).await {
        Ok(result) => result,
        Err(error) => {
            append_error(policy_error, error.to_string());
            return (false, false);
        }
    };
    update_observed_thresholds(
        device,
        result.actual_start_percent,
        result.actual_end_percent,
    );
    let battery_id = &device.id;
    if intent.restoration && !result.verified {
        append_error(
            policy_error,
            format!(
                "battery {battery_id} reported thresholds {}–{} after restoring {}–{}",
                result.actual_start_percent, result.actual_end_percent, target.0, target.1
            ),
        );
        return (false, false);
    }
    if intent.policy {
        let policy = config.device_mut(battery_id);
        policy.accepted_reported_start_percent = Some(result.actual_start_percent);
        policy.accepted_reported_end_percent = Some(result.actual_end_percent);
    }
    (true, intent.policy)
}

fn target_is_applied(
    observed: (Option<u8>, Option<u8>),
    target: (u8, u8),
    accepted: Option<(u8, u8)>,
    temporary_target: bool,
    policy_target: bool,
) -> bool {
    observed == (Some(target.0), Some(target.1))
        || (temporary_target && observed.1 == Some(100))
        || (policy_target
            && accepted.is_some_and(|accepted| observed == (Some(accepted.0), Some(accepted.1))))
}

fn reconciliation_target(
    device: &config::BatteryDeviceConfig,
    runtime: &config::BatteryRuntimeState,
    should_finish_charge_once: bool,
    battery_id: &str,
) -> Option<(u8, u8)> {
    let charge_once = runtime.charge_once_targets(battery_id);
    let safety_restoration = charge_once && should_finish_charge_once;
    if runtime
        .authorization_uncertain_batteries
        .iter()
        .any(|id| id == battery_id)
        && !safety_restoration
    {
        return None;
    }
    if runtime.operation == config::OperationKind::Calibration
        && runtime.operation_battery_id == battery_id
    {
        // Restoration is retried by reconcile_calibration, including readback
        // verification. Do not reopen 0–100 after an unsuccessful restoration.
        return (runtime.operation_phase != config::OperationPhase::Restoring).then_some((0, 100));
    }
    if charge_once && !should_finish_charge_once {
        return Some((0, 100));
    }
    let configured = device
        .manage_thresholds
        .then(|| device.desired_thresholds());
    if safety_restoration {
        return runtime
            .restore_start_percent
            .zip(runtime.restore_end_percent)
            .or(configured);
    }
    configured
}

async fn reconcile_runtime_operation(
    state: &mut BatteryState,
    runtime: &mut config::BatteryRuntimeState,
    policy_error: &mut Option<String>,
) -> bool {
    if runtime.operation == config::OperationKind::None {
        return false;
    }
    let Some(device) = state
        .devices
        .iter_mut()
        .find(|device| device.id == runtime.operation_battery_id)
    else {
        append_error(
            policy_error,
            format!(
                "cannot resume {} operation: battery {} is unavailable",
                runtime.operation, runtime.operation_battery_id
            ),
        );
        return false;
    };
    if runtime
        .authorization_uncertain_batteries
        .contains(&device.id)
    {
        if runtime.operation == config::OperationKind::Calibration {
            // Safety restoration, not a replay of the uncertain start.
            if finish_runtime_operation(device, runtime, policy_error).await {
                runtime
                    .authorization_uncertain_batteries
                    .retain(|id| id != &device.id);
                return true;
            }
        }
        return false;
    }
    match runtime.operation {
        config::OperationKind::Inhibit => {
            ensure_charge_behaviour(device, "inhibit-charge", policy_error).await;
            false
        }
        config::OperationKind::Calibration => {
            reconcile_calibration(device, state.plugged, runtime, policy_error).await
        }
        config::OperationKind::None => false,
    }
}

async fn reconcile_calibration(
    device: &mut BatteryDeviceState,
    plugged: bool,
    runtime: &mut config::BatteryRuntimeState,
    policy_error: &mut Option<String>,
) -> bool {
    if runtime.operation_phase == config::OperationPhase::Restoring
        || runtime.operation_expired(crate::time::unix_ms())
        || !plugged
    {
        return finish_runtime_operation(device, runtime, policy_error).await;
    }
    match runtime.operation_phase {
        config::OperationPhase::Discharging => {
            reconcile_calibration_discharge(device, runtime, policy_error).await
        }
        config::OperationPhase::Charging if device.percentage >= 100 => {
            finish_runtime_operation(device, runtime, policy_error).await
        }
        config::OperationPhase::Charging => {
            ensure_charge_behaviour(device, "auto", policy_error).await;
            false
        }
        phase => {
            append_error(policy_error, format!("invalid calibration phase: {phase}"));
            finish_runtime_operation(device, runtime, policy_error).await
        }
    }
}

async fn reconcile_calibration_discharge(
    device: &mut BatteryDeviceState,
    runtime: &mut config::BatteryRuntimeState,
    policy_error: &mut Option<String>,
) -> bool {
    if device.percentage > 1 {
        ensure_charge_behaviour(device, "force-discharge", policy_error).await;
        return false;
    }
    if ensure_charge_behaviour(device, "auto", policy_error).await {
        runtime.operation_phase = config::OperationPhase::Charging;
        return true;
    }
    false
}

async fn ensure_charge_behaviour(
    device: &mut BatteryDeviceState,
    desired: &str,
    policy_error: &mut Option<String>,
) -> bool {
    if device.protection.charge_behaviour.as_deref() == Some(desired) {
        return true;
    }
    match helper::set_charge_behaviour(&device.id, desired).await {
        Ok(actual) => {
            device.protection.charge_behaviour = Some(actual);
            true
        }
        Err(error) => {
            append_error(policy_error, error.to_string());
            false
        }
    }
}

async fn finish_runtime_operation(
    device: &mut BatteryDeviceState,
    runtime: &mut config::BatteryRuntimeState,
    policy_error: &mut Option<String>,
) -> bool {
    if runtime.operation_phase != config::OperationPhase::Restoring {
        runtime.operation_phase = config::OperationPhase::Restoring;
        if let Err(error) = config::save_runtime(runtime).await {
            append_error(policy_error, error.to_string());
            return false;
        }
    }
    let battery_id = &device.id;
    let behaviour_result = helper::set_charge_behaviour(battery_id, "auto").await;
    let threshold_result = match (
        runtime.operation_restore_start_percent,
        runtime.operation_restore_end_percent,
    ) {
        (Some(start), Some(end)) => helper::set_thresholds(battery_id, start, end).await,
        _ => Err(anyhow::anyhow!(
            "calibration restoration thresholds are missing"
        )),
    };
    match (behaviour_result, threshold_result) {
        (Ok(actual), Ok(thresholds)) => {
            device.protection.charge_behaviour = Some(actual);
            update_observed_thresholds(
                device,
                thresholds.actual_start_percent,
                thresholds.actual_end_percent,
            );
            match runtime.complete_calibration_restoration(
                thresholds.actual_start_percent,
                thresholds.actual_end_percent,
                thresholds.verified,
            ) {
                Ok(()) => true,
                Err(error) => {
                    append_error(policy_error, error.to_string());
                    false
                }
            }
        }
        (behaviour, thresholds) => {
            if let Err(error) = behaviour {
                append_error(policy_error, error.to_string());
            }
            if let Err(error) = thresholds {
                append_error(policy_error, error.to_string());
            }
            false
        }
    }
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
    let mut error = None;
    let config = policy_or_default(config::load_config().await, &mut error);
    let runtime = policy_or_default(config::load_runtime().await, &mut error);
    (config, runtime, error)
}

fn policy_or_default<T: Default>(result: Result<T>, error: &mut Option<String>) -> T {
    result.unwrap_or_else(|failure| {
        append_error(error, failure.to_string());
        T::default()
    })
}

pub(crate) fn decorate(
    mut state: BatteryState,
    config: &config::BatteryConfig,
    runtime: &config::BatteryRuntimeState,
    policy_error: Option<String>,
) -> BatteryState {
    state.warning = state.available && !state.plugged && state.percentage <= config.warning_percent;
    state.critical =
        state.available && !state.plugged && state.percentage <= config.critical_percent;
    state.policy = config.policy();
    state.operation = BatteryOperationState {
        kind: runtime.operation.as_str().into(),
        battery_id: runtime.operation_battery_id.clone(),
        phase: runtime.operation_phase.as_str().into(),
        started_unix_ms: runtime.operation_started_unix_ms,
        expires_unix_ms: runtime.operation_expires_unix_ms,
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
    crate::presentation::battery(&mut state);
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
    protection.thresholds_verified =
        protection.start_percent.zip(protection.end_percent) == Some(config.desired_thresholds());
    protection.charge_once_active = runtime.charge_once_targets(battery_id);
    if let Some(error) = policy_error {
        append_error(&mut protection.error, error);
    }
}

pub(crate) fn update_observed_thresholds(device: &mut BatteryDeviceState, start: u8, end: u8) {
    device.protection.start_percent = Some(start);
    device.protection.end_percent = Some(end);
    device.protection.enabled = start > 0 || end < 100;
}

fn semantic_state(state: &BatteryState) -> String {
    if state.operation.kind == "calibration" {
        return "calibrating".into();
    }
    if state.operation.kind == "inhibit" {
        return "charging-inhibited".into();
    }
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
    let state = history::attach_summary(state);
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
    fn uncertain_write_fence_survives_restart_and_allows_only_safety_restoration() {
        let device = config::BatteryDeviceConfig {
            manage_thresholds: true,
            protection_enabled: true,
            ..Default::default()
        };
        let mut runtime = config::BatteryRuntimeState::start_charge_once(1, "BAT0".into(), 60, 85);
        assert_eq!(
            reconciliation_target(&device, &runtime, false, "BAT0"),
            Some((0, 100))
        );
        runtime
            .authorization_uncertain_batteries
            .push("BAT0".into());
        let runtime: config::BatteryRuntimeState =
            serde_json::from_value(serde_json::to_value(runtime).unwrap()).unwrap();
        assert_eq!(
            reconciliation_target(&device, &runtime, false, "BAT0"),
            None
        );
        assert_eq!(
            reconciliation_target(&device, &runtime, true, "BAT0"),
            Some((60, 85))
        );
        assert_eq!(
            reconciliation_target(&device, &runtime, false, "BAT1"),
            Some((75, 80))
        );
    }

    #[test]
    fn pending_calibration_restoration_does_not_reopen_thresholds() {
        let device = config::BatteryDeviceConfig {
            manage_thresholds: true,
            protection_enabled: true,
            ..Default::default()
        };
        assert_eq!(
            reconciliation_target(
                &device,
                &config::BatteryRuntimeState::default(),
                false,
                "BAT0"
            ),
            Some((75, 80))
        );
        let mut runtime = config::BatteryRuntimeState::start_calibration(1, "BAT0".into(), 75, 80);
        assert_eq!(
            reconciliation_target(&device, &runtime, false, "BAT0"),
            Some((0, 100))
        );
        runtime.operation_phase = config::OperationPhase::Restoring;
        assert_eq!(
            reconciliation_target(&device, &runtime, false, "BAT0"),
            None
        );
    }
}
