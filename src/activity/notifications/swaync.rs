use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    time::sleep,
};

use crate::{model::NotificationState, state::StateStore};

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

pub async fn monitor(store: StateStore) {
    let mut retry = Duration::from_millis(1500);
    loop {
        match run_subscription(&store).await {
            Ok(had_value) => {
                if had_value {
                    retry = Duration::from_millis(1500);
                }
                store
                    .update_notifications(NotificationState {
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
        retry = (retry * 2).min(Duration::from_secs(30));
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
    if !status.success() {
        let stderr = child.stderr.take();
        let detail = if let Some(stderr) = stderr {
            let mut lines = BufReader::new(stderr).lines();
            lines.next_line().await.ok().flatten().unwrap_or_default()
        } else {
            String::new()
        };
        bail!("swaync-client exited with {status}: {detail}");
    }
    Ok(had_value)
}

fn parse_status(line: &str) -> Result<NotificationState> {
    let status: WaybarStatus =
        serde_json::from_str(line).context("decode swaync-client Waybar JSON")?;
    let normalized = format!("{} {}", status.alt, status.class_name).to_lowercase();
    Ok(NotificationState {
        available: true,
        count: notification_count(&status.text),
        dnd: normalized.contains("dnd") || normalized.contains("do-not-disturb"),
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

pub async fn toggle_panel() -> Result<()> {
    run_action("--toggle-panel").await
}
pub async fn toggle_dnd() -> Result<()> {
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
    use super::{notification_count, parse_status};

    #[test]
    fn parses_waybar_status_and_dnd_class() {
        let state = parse_status(r#"{"text":"3","alt":"dnd-notification","tooltip":"3 Notifications","class":"dnd-notification"}"#).unwrap();
        assert_eq!(state.count, 3);
        assert!(state.dnd);
        assert_eq!(state.tooltip, "3 Notifications");
    }

    #[test]
    fn tolerates_decorated_and_empty_counts() {
        assert_eq!(notification_count("󰂚 12"), 12);
        assert_eq!(notification_count("none"), 0);
    }
}
