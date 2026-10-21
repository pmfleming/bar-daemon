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

    match Cli::parse().command {
        Command::Daemon => run_daemon().await,
        Command::Client => run_client().await,
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
