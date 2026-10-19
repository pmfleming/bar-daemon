//! Session-independent system timezone authority; never changes UTC/NTP.
mod location;
#[path = "../../location_place.rs"]
mod location_place;

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Apply GeoClue timezones, confirming non-IP results with IP timezone agreement"
)]
struct Args {
    #[arg(long)]
    state_directory: Option<PathBuf>,
    #[arg(long)]
    runtime_directory: Option<PathBuf>,
    #[arg(long)]
    tzdata: Option<PathBuf>,
}
fn directory(value: Option<PathBuf>, environment: &str, fallback: &str) -> PathBuf {
    value
        .or_else(|| std::env::var_os(environment).map(PathBuf::from))
        .unwrap_or_else(|| fallback.into())
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    location::run(
        directory(
            args.state_directory,
            "STATE_DIRECTORY",
            "/var/lib/location-timezone",
        ),
        directory(
            args.runtime_directory,
            "RUNTIME_DIRECTORY",
            "/run/location-timezone",
        ),
        directory(args.tzdata, "TZDIR", "/usr/share/zoneinfo"),
    )
    .await
}
