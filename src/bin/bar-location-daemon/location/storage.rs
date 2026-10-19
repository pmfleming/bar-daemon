use super::policy::Source;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use shelllist_daemon_core::{AtomicFilePolicy, read_bytes_bounded};
use std::{path::PathBuf, time::Duration};

#[derive(Default, Deserialize, Serialize)]
pub(super) struct State {
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub ip_not_before: f64,
    #[serde(default)]
    pub wifi_not_before: f64,
    #[serde(default)]
    pub accepted: Option<Accepted>,
    // Persist intent before calling timedated: a crash or lost reply must not
    // silently re-arm writes on restart. Explicit reload clears this fence.
    #[serde(default)]
    pub write_pending: bool,
}
#[derive(Deserialize, Serialize)]
pub(super) struct Accepted {
    pub timezone: String,
    pub latitude: f64,
    pub longitude: f64,
    pub verified_at: f64,
    #[serde(default)]
    pub source: Source,
    #[serde(default)]
    pub accuracy_meters: Option<f64>,
}
impl State {
    pub fn geoclue_cooldown(&self, now: f64) -> Duration {
        Duration::from_secs_f64((self.wifi_not_before - now).clamp(0.0, 365.0 * 86400.0))
    }
}
pub(super) struct Storage {
    pub state_path: PathBuf,
    pub status_path: PathBuf,
    last_message: Option<String>,
}
impl Storage {
    pub fn new(state: PathBuf, runtime: PathBuf) -> Self {
        Self {
            state_path: state.join("state.json"),
            status_path: runtime.join("status.json"),
            last_message: None,
        }
    }
    pub fn load(&self) -> Result<State> {
        let state: State = match read_bytes_bounded(&self.state_path, 65536)? {
            Some(bytes) => {
                serde_json::from_slice(&bytes).context("invalid private location state")?
            }
            None => State::default(),
        };
        anyhow::ensure!(
            state.ip_not_before.is_finite()
                && state.ip_not_before >= 0.0
                && state.wifi_not_before.is_finite()
                && state.wifi_not_before >= 0.0,
            "invalid provider cooldown"
        );
        Ok(state)
    }
    pub async fn save(&self, state: &State) -> Result<()> {
        shelllist_daemon_tokio::write_bytes_atomic_async(
            self.state_path.clone(),
            serde_json::to_vec(state)?,
            AtomicFilePolicy::PRIVATE,
        )
        .await?;
        Ok(())
    }
    pub async fn status(&mut self, message: &str) -> Result<()> {
        if self.last_message.as_deref() == Some(message) {
            return Ok(());
        }
        shelllist_daemon_tokio::write_bytes_atomic_async(
            self.status_path.clone(),
            serde_json::to_vec(&serde_json::json!({"message": message}))?,
            AtomicFilePolicy {
                directory_mode: Some(0o755),
                file_mode: Some(0o644),
                ..AtomicFilePolicy::PRIVATE
            },
        )
        .await?;
        self.last_message = Some(message.to_owned());
        tracing::info!(message, "location status");
        Ok(())
    }
}
