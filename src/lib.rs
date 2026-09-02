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
mod state;
mod time;
mod timezone;
mod updates;

/// Runs the session D-Bus daemon until it receives a termination signal.
pub async fn run_daemon() -> anyhow::Result<()> {
    daemon::run().await
}

/// Runs the JSON Lines bridge between standard I/O and the session daemon.
pub async fn run_client() -> anyhow::Result<()> {
    client::run().await
}

/// Runs the privileged battery helper service.
pub async fn run_battery_helper() -> anyhow::Result<()> {
    battery::helper::run().await
}
