use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    time::sleep,
};

use crate::{model::NotificationState, state::StateStore};

const INITIAL_RETRY: Duration = Duration::from_millis(1500);
const MAXIMUM_RETRY: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Deserialize)]
struct WaybarStatus {
    #[serde(default)]
    text: String,
    #[serde(default)]
    alt: String,
    #[serde(default)]
    tooltip: String,
    #[serde(default, rename = "class")]
    class_name: String,
}

pub(crate) async fn monitor(store: StateStore) {
    let mut retry = INITIAL_RETRY;
    loop {
        match run_subscription(&store).await {
            Ok(had_value) => {
                if had_value {
                    retry = INITIAL_RETRY;
                }
                store
                    .update_notifications(NotificationState {
                        available: false,
                        error: Some("SwayNC subscription ended".into()),
                        ..store.snapshot().await.notifications
                    })
                    .await;
            }
            Err(error) => {
                store
                    .update_notifications(NotificationState {
                        error: Some(error.to_string()),
                        ..NotificationState::default()
                    })
                    .await;
            }
        }
        sleep(retry).await;
        retry = next_retry(retry);
    }
}

async fn run_subscription(store: &StateStore) -> Result<bool> {
    let mut child = Command::new("swaync-client")
        .arg("--subscribe-waybar")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("start swaync-client subscription")?;
    let stdout = child
        .stdout
        .take()
        .context("capture swaync-client stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("capture swaync-client stderr")?;
    let stderr_task = tokio::spawn(async move {
        let mut detail = String::new();
        BufReader::new(stderr)
            .take(64 * 1024)
            .read_to_string(&mut detail)
            .await
            .map(|_| detail)
    });
    let mut lines = BufReader::new(stdout).lines();
    let mut had_value = false;
    while let Some(line) = lines.next_line().await.context("read SwayNC status")? {
        if line.trim().is_empty() {
            continue;
        }
        match parse_status(&line) {
            Ok(state) => {
                had_value = true;
                store.update_notifications(state).await;
            }
            Err(error) => tracing::warn!(%error, line = %line, "ignored invalid SwayNC status"),
        }
    }
    let status = child.wait().await.context("wait for swaync-client")?;
    let detail = stderr_task
        .await
        .context("join swaync-client stderr reader")??;
    if !status.success() {
        bail!("swaync-client exited with {status}: {}", detail.trim());
    }
    Ok(had_value)
}

fn next_retry(current: Duration) -> Duration {
    (current * 2).min(MAXIMUM_RETRY)
}

fn parse_status(line: &str) -> Result<NotificationState> {
    let status: WaybarStatus =
        serde_json::from_str(line).context("decode swaync-client Waybar JSON")?;
    let normalized = format!("{} {}", status.alt, status.class_name).to_lowercase();
    Ok(NotificationState {
        available: true,
        count: notification_count(&status.text),
        dnd: normalized.contains("dnd") || normalized.contains("do-not-disturb"),
        dnd_until_unix_ms: None,
        inhibited: normalized.contains("inhibited"),
        text: status.text,
        tooltip: status.tooltip,
        alt: status.alt,
        class_name: status.class_name,
        backend: "swaync".into(),
        history_revision: 0,
        error: None,
    })
}

fn notification_count(text: &str) -> u32 {
    text.split(|ch: char| !ch.is_ascii_digit())
        .find(|part| !part.is_empty())
        .and_then(|part| part.parse().ok())
        .unwrap_or(0)
}

pub(crate) async fn toggle_panel() -> Result<()> {
    run_action("--toggle-panel").await
}
pub(crate) async fn toggle_dnd() -> Result<()> {
    run_action("--toggle-dnd").await
}

async fn run_action(action: &str) -> Result<()> {
    let output = Command::new("swaync-client")
        .args([action, "--skip-wait"])
        .output()
        .await
        .context("start swaync-client action")?;
    if !output.status.success() {
        bail!(
            "swaync-client {action} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{next_retry, notification_count, parse_status};

    #[test]
    fn parses_waybar_status_and_dnd_class() {
        let state = parse_status(r#"{"text":"3","alt":"dnd-notification","tooltip":"3 Notifications","class":"dnd-notification"}"#).unwrap();
        assert_eq!(state.count, 3);
        assert!(state.dnd);
        assert_eq!(state.tooltip, "3 Notifications");
    }

    #[test]
    fn caps_subscription_retry_backoff() {
        assert_eq!(
            next_retry(Duration::from_millis(1500)),
            Duration::from_secs(3)
        );
        assert_eq!(next_retry(Duration::from_secs(20)), Duration::from_secs(30));
        assert_eq!(next_retry(Duration::from_secs(30)), Duration::from_secs(30));
    }

    #[test]
    fn tolerates_decorated_and_empty_counts() {
        assert_eq!(notification_count("󰂚 12"), 12);
        assert_eq!(notification_count("none"), 0);
    }
}
