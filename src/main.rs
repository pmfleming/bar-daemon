use std::io::IsTerminal;

use anyhow::Result;
use bar_daemon::{protocol, run_client, run_daemon};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the session D-Bus service.
    Daemon,
    /// Bridge JSON Lines on stdin/stdout to the session service.
    Client,
    /// Run hypridle with the selected persistent sleep profile.
    Idle {
        #[arg(long)]
        config: std::path::PathBuf,
        #[arg(long)]
        hypridle: std::path::PathBuf,
    },
    /// Apply the current automatic sleep policy (called by hypridle).
    IdleSleep {
        #[arg(long)]
        sleep_minutes: u32,
        #[arg(long)]
        generation: String,
        #[arg(long)]
        episode: u64,
    },
    /// Offline Agenda export/restore. Stop the daemon manually first; no service control.
    Agenda {
        #[command(subcommand)]
        command: bar_daemon::AgendaMaintenance,
    },
    /// Print stable protocol metadata or a contract fixture.
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
}

#[derive(Debug, Subcommand)]
enum DebugCommand {
    ProtocolRegistry,
    ContractFixture,
    ReadContractFixture,
    ControlContractFixture,
    SleepContractFixture,
    DisplayContractFixture,
    NotificationContractFixture,
    SessionControlContractFixture,
    BatteryContractFixture,
    ActivityContractFixture,
    QueryContractFixture,
    /// Read worker/manual rebuild history without updating or notifying.
    UpdateHistory,
    /// Read kernel/swap/ThinkPad sleep evidence without initiating sleep.
    SleepDiagnostics,
    /// Observe compositor lock confirmation without requesting a lock or sleep.
    LockState,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("bar_daemon=info")),
        )
        .init();

    dispatch(Cli::parse().command).await
}

async fn dispatch(command: Command) -> Result<()> {
    match command {
        Command::Daemon => run_daemon().await,
        Command::Client => run_client().await,
        Command::Agenda { command } => bar_daemon::run_agenda_maintenance(command).await,
        Command::Idle { config, hypridle } => bar_daemon::run_idle(&config, &hypridle).await,
        Command::IdleSleep {
            sleep_minutes,
            generation,
            episode,
        } => bar_daemon::run_idle_sleep(sleep_minutes, &generation, episode).await,
        Command::Debug { command } => {
            let value = match command {
                DebugCommand::ProtocolRegistry => protocol::registry(),
                DebugCommand::ContractFixture => protocol::contract_fixture()?,
                DebugCommand::ReadContractFixture => protocol::read_contract_fixture().await?,
                DebugCommand::ControlContractFixture => protocol::control_contract_fixture(),
                DebugCommand::SleepContractFixture => protocol::sleep_contract_fixture().await,
                DebugCommand::DisplayContractFixture => protocol::display_contract_fixture().await,
                DebugCommand::NotificationContractFixture => {
                    protocol::notification_contract_fixture().await
                }
                DebugCommand::SessionControlContractFixture => {
                    protocol::session_control_contract_fixture().await
                }
                DebugCommand::BatteryContractFixture => protocol::battery_contract_fixture().await,
                DebugCommand::ActivityContractFixture => {
                    protocol::activity_contract_fixture().await
                }
                DebugCommand::QueryContractFixture => protocol::query_contract_fixture().await,
                DebugCommand::UpdateHistory => bar_daemon::update_history().await?,
                DebugCommand::SleepDiagnostics => bar_daemon::sleep_diagnostics(),
                DebugCommand::LockState => bar_daemon::inspect_lock().await?,
            };
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn agenda_routes_require_explicit_paths_and_reject_wrong_flags() {
        for args in [
            vec!["bar-daemon", "agenda"],
            vec!["bar-daemon", "agenda", "export"],
            vec!["bar-daemon", "agenda", "export", "--store-dir", "/private"],
            vec!["bar-daemon", "agenda", "restore", "--input", "/backup"],
            vec![
                "bar-daemon",
                "agenda",
                "restore",
                "--store-dir",
                "/private",
                "--output",
                "/backup",
            ],
            vec![
                "bar-daemon",
                "agenda",
                "export",
                "--store-dir",
                "/private",
                "--output",
                "/backup",
                "--force",
            ],
        ] {
            assert!(
                Cli::try_parse_from(&args).is_err(),
                "unexpected acceptance: {args:?}"
            );
        }
        assert!(matches!(
            Cli::try_parse_from(["bar-daemon", "daemon"])
                .unwrap()
                .command,
            Command::Daemon
        ));
        assert!(matches!(
            Cli::try_parse_from(["bar-daemon", "client"])
                .unwrap()
                .command,
            Command::Client
        ));
    }

    #[tokio::test]
    async fn agenda_cli_parse_dispatch_and_real_store_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let input = temp.path().join("input.json");
        let source = serde_json::json!({
            "application_id": 0x41474e44, "schema": 1,
            "snapshot": {"items": [], "catalog_revision": 0},
            "sources": [["local",1,1,1]], "receipts": [], "migrations": []
        });
        fs::write(&input, serde_json::to_vec(&source).unwrap()).unwrap();
        fs::set_permissions(&input, fs::Permissions::from_mode(0o600)).unwrap();
        let root = temp.path().join("restored");
        let output = temp.path().join("output.json");
        let restore = || {
            Cli::try_parse_from([
                "bar-daemon",
                "agenda",
                "restore",
                "--store-dir",
                root.to_str().unwrap(),
                "--input",
                input.to_str().unwrap(),
            ])
            .unwrap()
            .command
        };
        dispatch(restore()).await.unwrap();
        assert!(dispatch(restore()).await.is_err());
        let export = || {
            Cli::try_parse_from([
                "bar-daemon",
                "agenda",
                "export",
                "--store-dir",
                root.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ])
            .unwrap()
            .command
        };
        dispatch(export()).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&output).unwrap()).unwrap(),
            source
        );
        assert!(dispatch(export()).await.is_err());
        let missing = temp.path().join("missing");
        let command = Cli::try_parse_from([
            "bar-daemon",
            "agenda",
            "export",
            "--store-dir",
            missing.to_str().unwrap(),
            "--output",
            temp.path().join("must-not-exist").to_str().unwrap(),
        ])
        .unwrap()
        .command;
        assert!(dispatch(command).await.is_err());
        assert!(!missing.exists());
        assert!(!temp.path().join("must-not-exist").exists());
    }
}
