//! Own only logind's low-level lid switch, never bypass sleep inhibitors.
//! The FD is released on disconnect, task cancellation, inactive session, or
//! System policy, restoring the administrator's logind policy automatically.
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::{MissedTickBehavior, interval, sleep};

use super::{POLICY_WRITE, SleepPolicy, base_config, load, perform_profile, plugged};
use crate::{sleep as power_sleep, state::StateStore};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum LidAction {
    #[default]
    System,
    Ignore,
    Lock,
    Suspend,
    Hibernate,
    Profile,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct LidState {
    pub available: bool,
    pub managed: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct LidEdge {
    previous: Option<bool>,
    eligible: bool,
}

impl LidEdge {
    fn observe(&mut self, closed: bool, eligible: bool) -> bool {
        // Startup, reconnect, undocking, and switching sessions are baselines,
        // not new lid closes. Never sleep just because a lid is already closed.
        let triggered = eligible && self.eligible && self.previous == Some(false) && closed;
        self.previous = Some(closed);
        self.eligible = eligible;
        triggered
    }
}

pub(crate) async fn monitor(store: StateStore) {
    // No implicit takeover when the optional managed integration is disabled.
    if base_config().is_err() {
        return;
    }
    loop {
        if let Err(error) = connected(&store).await {
            store
                .update_lid(LidState {
                    error: Some(format!(
                        "Lid control unavailable; system policy applies: {error:#}"
                    )),
                    ..Default::default()
                })
                .await;
        }
        sleep(Duration::from_secs(3)).await;
    }
}

async fn connected(store: &StateStore) -> Result<()> {
    let connection = power_sleep::system_bus().await?;
    let manager = power_sleep::manager(&connection).await?;
    let session = power_sleep::current_session(&connection).await?;
    let upower = zbus::Proxy::new(
        &connection,
        "org.freedesktop.UPower",
        "/org/freedesktop/UPower",
        "org.freedesktop.UPower",
    )
    .await?;
    let present: bool = upower
        .get_property("LidIsPresent")
        .await
        .context("read lid presence")?;
    if !present {
        bail!("no laptop lid was detected");
    }
    let properties = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.DBus.Properties",
    )
    .await?;
    let session_properties = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        session.path().clone(),
        "org.freedesktop.DBus.Properties",
    )
    .await?;
    let mut owner_changes = manager.receive_owner_changed().await?;
    let mut changes = properties.receive_signal("PropertiesChanged").await?;
    let mut session_changes = session_properties
        .receive_signal("PropertiesChanged")
        .await?;
    let mut policies = store.subscribe();
    let mut fallback = interval(Duration::from_secs(2));
    fallback.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut inhibitor: Option<zvariant::OwnedFd> = None;
    let mut edge = LidEdge::default();
    // JoinSet cancellation drops the action when this connection/owner is lost.
    // The observer never waits for POLICY_WRITE or for locking/helper setup.
    let mut actions = tokio::task::JoinSet::new();
    let mut status = LidState {
        available: true,
        ..Default::default()
    };
    store.update_lid(status.clone()).await;
    loop {
        tokio::select! {
            result = actions.join_next(), if !actions.is_empty() => {
                match result.context("lid action worker ended")? {
                    Ok(Ok(Some(state))) => { store.update_power_sleep(state).await; status.error = None; }
                    Ok(Ok(None)) => { status.error = None; }
                    Ok(Err(error)) => status.error = Some(format!("Lid action failed: {error:#}")),
                    Err(error) => status.error = Some(format!("Lid action worker failed: {error}")),
                }
                store.update_lid(status.clone()).await;
                continue;
            }
            _ = fallback.tick() => {},
            _ = owner_changes.next() => bail!("logind restarted; reacquiring lid ownership"),
            signal = changes.next() => { if signal.is_none() { bail!("lid signal stream ended"); } },
            signal = session_changes.next() => { if signal.is_none() { bail!("session signal stream ended"); } },
            event = policies.recv() => match event {
                Ok(event) if event.stream == crate::protocol::stream::SLEEP_POLICY => {},
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                _ => continue,
            }
        }
        let policy =
            power_sleep::bounded("read lid policy", Duration::from_secs(2), load()).await?;
        let active = active_local_graphical(&session).await?;
        let managed = policy.lid_action != LidAction::System && active;
        if managed && inhibitor.is_none() {
            inhibitor = Some(
                manager
                    .call(
                        "Inhibit",
                        &(
                            "handle-lid-switch",
                            "Shelllist",
                            "User-selected lid-close policy",
                            "block",
                        ),
                    )
                    .await
                    .context("take ownership of the lid switch")?,
            );
        } else if !managed {
            inhibitor = None;
        }
        let closed: bool = manager.get_property("LidClosed").await?;
        // logind's Docked also covers external displays. Keep clamshell use safe.
        let docked: bool = manager.get_property("Docked").await?;
        let triggered = edge.observe(closed, managed && !docked);
        if triggered && actions.is_empty() {
            actions.spawn(async move {
                let _guard = tokio::time::timeout(Duration::from_secs(3), POLICY_WRITE.lock())
                    .await
                    .context("lid policy is busy; ignoring this close")?;
                if load().await? != policy {
                    bail!("lid policy changed before action");
                }
                let connection = power_sleep::system_bus().await?;
                let manager = power_sleep::manager(&connection).await?;
                let session = power_sleep::current_session(&connection).await?;
                act(&policy, &manager, &session).await
            });
        }
        status.managed = managed;
        store.update_lid(status.clone()).await;
    }
}

async fn active_local_graphical(session: &zbus::Proxy<'_>) -> Result<bool> {
    let active: bool = session.get_property("Active").await?;
    let remote: bool = session.get_property("Remote").await?;
    let kind: String = session.get_property("Type").await?;
    Ok(active && !remote && matches!(kind.as_str(), "wayland" | "x11"))
}

async fn confirm_trigger(manager: &zbus::Proxy<'_>, session: &zbus::Proxy<'_>) -> Result<()> {
    if !active_local_graphical(session).await?
        || !manager.get_property::<bool>("LidClosed").await?
        || manager.get_property::<bool>("Docked").await?
    {
        bail!("lid reopened, dock connected, or graphical session became inactive");
    }
    Ok(())
}

async fn act(
    policy: &SleepPolicy,
    manager: &zbus::Proxy<'_>,
    session: &zbus::Proxy<'_>,
) -> Result<Option<crate::model::PowerSleepState>> {
    confirm_trigger(manager, session).await?;
    let state = match policy.lid_action {
        LidAction::System | LidAction::Ignore => return Ok(None),
        LidAction::Lock => power_sleep::perform("lock").await?,
        LidAction::Suspend | LidAction::Hibernate => {
            let action = if policy.lid_action == LidAction::Suspend {
                "suspend"
            } else {
                "hibernate"
            };
            power_sleep::perform_with_setup(action, || confirm_trigger(manager, session)).await?
        }
        LidAction::Profile => {
            let profile = policy.profile(plugged().await?);
            perform_profile(profile, || confirm_trigger(manager, session)).await?
        }
    };
    Ok(Some(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn policy_transaction_does_not_block_lid_ownership_publication() {
        let _busy = POLICY_WRITE.lock().await;
        let store = StateStore::default();
        tokio::time::timeout(
            Duration::from_millis(50),
            store.update_lid(LidState {
                available: true,
                managed: false,
                error: None,
            }),
        )
        .await
        .unwrap();
        assert!(store.snapshot().await.sleep_policy.lid.available);
    }

    #[test]
    fn old_policies_preserve_system_behavior_and_unknown_actions_are_rejected() {
        let old = r#"{"same_profile":true,"battery":{"sleep_minutes":30,"hibernate_minutes":0},"plugged":{"sleep_minutes":30,"hibernate_minutes":0}}"#;
        assert_eq!(
            serde_json::from_str::<SleepPolicy>(old).unwrap().lid_action,
            LidAction::System
        );
        assert!(serde_json::from_str::<LidAction>("\"shutdown\"").is_err());
        for action in [
            LidAction::System,
            LidAction::Ignore,
            LidAction::Lock,
            LidAction::Suspend,
            LidAction::Hibernate,
            LidAction::Profile,
        ] {
            assert_eq!(
                serde_json::from_str::<LidAction>(&serde_json::to_string(&action).unwrap())
                    .unwrap(),
                action
            );
        }
    }

    #[test]
    fn acts_once_per_close_and_never_on_startup_resume_or_session_switch() {
        let mut edge = LidEdge::default();
        assert!(!edge.observe(true, true));
        assert!(!edge.observe(true, true));
        assert!(!edge.observe(false, true));
        assert!(edge.observe(true, true));
        assert!(!edge.observe(true, true));
        assert!(!edge.observe(false, false)); // docked/inactive/System
        assert!(!edge.observe(true, false));
        assert!(!edge.observe(true, true)); // undock/reactivate with lid closed
        assert!(!edge.observe(false, true));
        assert!(edge.observe(true, true));
    }

    #[test]
    fn lid_profile_uses_ac_delay_even_when_idle_sleep_is_never() {
        let policy = SleepPolicy {
            lid_action: LidAction::Profile,
            same_profile: false,
            battery: super::super::SleepProfile {
                sleep_minutes: 0,
                hibernate_minutes: 15,
            },
            plugged: super::super::SleepProfile {
                sleep_minutes: 0,
                hibernate_minutes: 120,
            },
        };
        assert_eq!(policy.profile(false).hibernate_minutes, 15);
        assert_eq!(policy.profile(true).hibernate_minutes, 120);
        assert!(policy.idle_profile(false, 0).is_err());
    }
}
