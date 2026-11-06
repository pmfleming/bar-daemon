#![recursion_limit = "256"]

//! Session integration, policy, and action service for a Quickshell desktop bar.
//!
//! Most implementation modules are private. The supported library surface is limited to
//! process entry points and the versioned protocol registry.

mod activity;
mod agenda_maintenance;
mod api;
mod audio;
mod battery;
mod brightness;
mod bus;
mod client;
pub mod component;
mod compositor;
mod daemon;
mod desktop_identity;
mod display_policy;
mod hyprland;
use bar_location_api as location_place;
mod media;
mod model;
mod osd_hardware;
mod paths;
mod power;
mod presentation;
/// Stable metadata and contract fixtures for the JSON/D-Bus protocol.
pub mod protocol;
mod session_sleep;
mod sleep;
mod sleep_policy;
mod state;
mod theme;
mod time;
mod timezone;
mod timezone_regions;
mod updates;
mod work_area;
mod workspace;

#[cfg(test)]
mod frontend_contract;
#[cfg(test)]
mod test_support;

pub use agenda_maintenance::AgendaMaintenance;
pub use workspace::{CommandKind as WorkspaceCommand, run_command as run_workspace_command};

/// Transfers an explicit private Agenda store offline; never controls services.
pub async fn run_agenda_maintenance(command: AgendaMaintenance) -> anyhow::Result<()> {
    command.run().await
}

/// Runs the session D-Bus daemon until it receives a termination signal.
pub async fn run_daemon(native: bar_native_api::NativeProviders) -> anyhow::Result<()> {
    daemon::run_bar(native).await
}

/// Runs the JSON Lines bridge between standard I/O and the session daemon.
pub async fn run_client() -> anyhow::Result<()> {
    client::run().await
}

/// Generates the selected sleep profile and replaces this process with hypridle.
pub async fn run_idle(config: &std::path::Path, hypridle: &std::path::Path) -> anyhow::Result<()> {
    sleep_policy::run(config, hypridle).await
}

/// Requests the current idle policy through the resident service.
pub async fn run_idle_sleep(
    sleep_minutes: u32,
    generation: &str,
    episode: u64,
) -> anyhow::Result<()> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        component::Component::Power.bus_name(),
        api::OBJECT_PATH,
        api::INTERFACE,
    )
    .await?;
    let params =
        serde_json::json!({"sleep_minutes": sleep_minutes, "generation": generation, "episode": episode}).to_string();
    let reply: String = proxy.call("Call", &("powerSleep.idle", params)).await?;
    let reply: serde_json::Value = serde_json::from_str(&reply)?;
    if reply["ok"] != true {
        anyhow::bail!("automatic sleep failed: {}", reply["error"]);
    }
    Ok(())
}

/// Read the same bounded update catalog as the UI without sending notifications.
pub async fn update_history() -> anyhow::Result<serde_json::Value> {
    updates::history::query(serde_json::json!({})).await
}

/// Reports read-only kernel, swap and Lenovo/ThinkPad sleep diagnostics.
pub fn sleep_diagnostics() -> serde_json::Value {
    serde_json::to_value(sleep::diagnostics::system()).expect("sleep diagnostics are serializable")
}

/// Reads the compositor's lock-notification state without changing it.
pub async fn inspect_lock() -> anyhow::Result<serde_json::Value> {
    Ok(
        serde_json::json!({"source": "hyprland-lock-notify-v1", "locked": sleep::inspect_lock().await?}),
    )
}

/// Run the power owner with a statically selected policy assembly.
pub async fn run_power_daemon(native: bar_native_api::NativeProviders) -> anyhow::Result<()> {
    daemon::run_power(native).await
}
/// Run the compositor owner without unrelated native providers.
pub async fn run_compositor_daemon(native: bar_native_api::NativeProviders) -> anyhow::Result<()> {
    daemon::run_compositor(native).await
}
/// Run the activity owner without unrelated native providers.
pub async fn run_activity_daemon(native: bar_native_api::NativeProviders) -> anyhow::Result<()> {
    daemon::run_activity(native).await
}
/// Run the notification owner with its native sharing provider.
pub async fn run_notification_daemon(
    native: bar_native_api::NativeProviders,
) -> anyhow::Result<()> {
    daemon::run_notifications(native).await
}

/// Pure native-provider interfaces consumed by the owner libraries.
pub use bar_native_api::NativeProviders;
