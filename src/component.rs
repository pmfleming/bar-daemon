//! Process ownership. Wire payloads stay versioned by the shared bar-api registry.
use crate::protocol::stream;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Bar,
    Notifications,
}
impl Component {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bar => "bar-daemon",
            Self::Notifications => "bar-notification-daemon",
        }
    }
    pub const fn bus_name(self) -> &'static str {
        match self {
            Self::Bar => "org.laufan.BarDaemon",
            Self::Notifications => "org.laufan.BarNotificationDaemon",
        }
    }
    pub(crate) fn owns_stream(self, name: &str) -> bool {
        let notifications = matches!(name, stream::NOTIFICATIONS | stream::NOTIFICATION_ACTIVE);
        match self {
            Self::Bar => !notifications,
            Self::Notifications => notifications,
        }
    }
    pub(crate) fn owns_method(self, name: &str) -> bool {
        if name == "bar.snapshot" {
            return true;
        }
        let notifications = name.starts_with("notifications.");
        match self {
            Self::Bar => !notifications,
            Self::Notifications => notifications,
        }
    }
    pub(crate) fn streams(self) -> Vec<&'static str> {
        crate::protocol::STREAMS
            .iter()
            .copied()
            .filter(|s| self.owns_stream(s))
            .collect()
    }
}

/// Entrypoint shared by the component executables. Each has its own endpoint.
pub async fn main(component: Component) -> anyhow::Result<()> {
    use clap::{Parser, Subcommand};
    use std::io::IsTerminal;
    #[derive(Parser)]
    #[command(version)]
    struct Args {
        #[command(subcommand)]
        command: Command,
    }
    #[derive(Subcommand)]
    enum Command {
        Daemon,
        Client,
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    match Args::parse().command {
        Command::Daemon => crate::daemon::run_component(component).await,
        Command::Client => crate::client::run_component(component).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streams_have_one_process_owner() {
        for stream in crate::protocol::STREAMS {
            assert_eq!(
                [Component::Bar, Component::Notifications]
                    .iter()
                    .filter(|c| c.owns_stream(stream))
                    .count(),
                1,
                "{stream}"
            );
        }
        assert!(!Component::Bar.owns_method("notifications.invoke"));
        assert!(!Component::Notifications.owns_method("powerSleep.request"));
    }
    #[tokio::test]
    async fn component_snapshots_and_calls_reject_foreign_domains() {
        for component in [Component::Bar, Component::Notifications] {
            let state = crate::state::StateStore::for_component(component);
            let api = crate::api::ApiService::new(
                state.clone(),
                None,
                crate::activity::notifications::service::NotificationService::remote(),
                crate::media::MediaService::default(),
                crate::brightness::BrightnessService::new(state.clone()),
            );
            let value = api.dispatch("bar.snapshot", serde_json::json!({})).await;
            let keys = value["data"]["snapshot"].as_object().unwrap();
            assert_eq!(keys.len(), component.streams().len());
            assert_eq!(
                keys.contains_key("notifications"),
                component == Component::Notifications
            );
            let foreign = if component == Component::Notifications {
                "battery.changed"
            } else {
                "notifications.changed"
            };
            assert_eq!(
                api.dispatch("bar.snapshot", serde_json::json!({"streams":[foreign]}))
                    .await["ok"],
                false
            );
        }
    }
}
