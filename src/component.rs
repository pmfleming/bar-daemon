//! Process ownership. Wire payloads stay versioned by the shared bar-api registry.
use crate::protocol::stream;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Bar,
    Notifications,
    Power,
    Compositor,
    Activity,
}
impl Component {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bar => "bar-daemon",
            Self::Notifications => "bar-notification-daemon",
            Self::Power => "bar-power-daemon",
            Self::Compositor => "bar-compositor-daemon",
            Self::Activity => "bar-activity-daemon",
        }
    }
    pub const fn bus_name(self) -> &'static str {
        match self {
            Self::Bar => "org.laufan.BarStatusDaemon",
            Self::Notifications => "org.laufan.BarNotificationDaemon",
            Self::Power => "org.laufan.BarPowerDaemon",
            Self::Compositor => "org.laufan.BarCompositorDaemon",
            Self::Activity => "org.laufan.BarActivityDaemon",
        }
    }
    // The 4 MiB catalog budget counts decoded strings. JSON escaping can grow
    // those strings sixfold; leave bounded room for fields and the outer record.
    pub(crate) const fn request_bytes(self) -> usize {
        if matches!(self, Self::Bar) {
            32 * 1024 * 1024
        } else {
            1024 * 1024
        }
    }
    pub(crate) fn owns_stream(self, name: &str) -> bool {
        let owner = match name {
            stream::NOTIFICATIONS | stream::NOTIFICATION_ACTIVE => Self::Notifications,
            stream::BATTERY
            | stream::POWER_PROFILE
            | stream::POWER_SLEEP
            | stream::SLEEP_POLICY => Self::Power,
            stream::WORKSPACES | stream::WORKAREA | stream::COMPOSITOR | stream::DISPLAY_POLICY => {
                Self::Compositor
            }
            stream::ACTIVITY | stream::TIMEZONE => Self::Activity,
            _ => Self::Bar,
        };
        self == owner
    }
    pub(crate) fn owns_method(self, name: &str) -> bool {
        if name == "bar.snapshot" {
            return true;
        }
        if ["notifications."]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            return self == Self::Notifications;
        }
        if ["battery.", "powerProfile.", "powerSleep."]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            return self == Self::Power;
        }
        if [
            "workspace.",
            "displayPolicy.",
            "displayFocus.",
            "displayLayout.",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        {
            return self == Self::Compositor;
        }
        if ["agenda.", "activity.", "locations.", "location.", "todos."]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            return self == Self::Activity;
        }
        self == Self::Bar
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
                [
                    Component::Bar,
                    Component::Notifications,
                    Component::Power,
                    Component::Compositor,
                    Component::Activity
                ]
                .iter()
                .filter(|c| c.owns_stream(stream))
                .count(),
                1,
                "{stream}"
            );
        }
        for method in crate::protocol::METHODS
            .iter()
            .filter(|m| **m != "bar.snapshot")
        {
            assert_eq!(
                [
                    Component::Bar,
                    Component::Notifications,
                    Component::Power,
                    Component::Compositor,
                    Component::Activity
                ]
                .iter()
                .filter(|c| c.owns_method(method))
                .count(),
                1,
                "{method}"
            );
        }
        assert!(!Component::Bar.owns_method("notifications.invoke"));
        assert!(!Component::Notifications.owns_method("powerSleep.request"));
    }
    #[tokio::test]
    async fn component_snapshots_and_calls_reject_foreign_domains() {
        for component in [
            Component::Bar,
            Component::Notifications,
            Component::Power,
            Component::Compositor,
            Component::Activity,
        ] {
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
