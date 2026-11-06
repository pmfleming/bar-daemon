//! Public process/contract facade; domain assemblies and native providers are workspace crates.
pub use bar_daemon_core::{
    AgendaMaintenance, WorkspaceCommand, component, inspect_lock, protocol, run_agenda_maintenance,
    run_client, run_idle, run_idle_sleep, run_workspace_command, sleep_diagnostics, update_history,
};
pub use bar_status::run as run_daemon;
