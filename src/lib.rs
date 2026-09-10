#![recursion_limit = "256"]

//! Session integration, policy, and action service for a Quickshell desktop bar.
//!
//! Most implementation modules are private. The supported library surface is limited to
//! process entry points and the versioned protocol registry.

mod activity;
mod api;
mod audio;
mod battery;
mod brightness;
mod client;
mod daemon;
mod hyprland;
mod media;
mod model;
mod osd_hardware;
mod paths;
mod power;
/// Stable metadata and contract fixtures for the JSON/D-Bus protocol.
pub mod protocol;
mod sleep;
mod sleep_policy;
mod state;
mod time;
mod timezone;
mod timezone_regions;
mod updates;

/// Runs the session D-Bus daemon until it receives a termination signal.
pub async fn run_daemon() -> anyhow::Result<()> {
    daemon::run().await
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
pub async fn run_idle_sleep(sleep_minutes: u32, generation: &str) -> anyhow::Result<()> {
    let connection = zbus::Connection::session().await?;
    let proxy =
        zbus::Proxy::new(&connection, api::BUS_NAME, api::OBJECT_PATH, api::INTERFACE).await?;
    let params =
        serde_json::json!({"sleep_minutes": sleep_minutes, "generation": generation}).to_string();
    let reply: String = proxy.call("Call", &("powerSleep.idle", params)).await?;
    let reply: serde_json::Value = serde_json::from_str(&reply)?;
    if reply["ok"] != true {
        anyhow::bail!("automatic sleep failed: {}", reply["error"]);
    }
    Ok(())
}

/// Runs the privileged battery helper service.
pub async fn run_battery_helper() -> anyhow::Result<()> {
    battery::helper::run().await
}
