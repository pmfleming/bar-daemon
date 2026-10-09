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
    let mut owner_changes = manager.receive_owner_changed().await?;
    let mut changes = power_sleep::property_changes(&manager).await?;
    let mut session_changes = power_sleep::property_changes(&session).await?;
    let mut policies = store.subscribe();
    let mut fallback = interval(Duration::from_secs(2));
    fallback.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut inhibitor: Option<zvariant::OwnedFd> = None;
    let mut edge = LidEdge::default();
    // JoinSet cancellation drops the action when this connection/owner is lost.
    // The observer never waits for POLICY_WRITE or for locking/helper setup.
    let mut actions = tokio::task::JoinSet::new();
    let mut retry = LidRetry::default();
    let mut status = LidState {
        available: true,
        ..Default::default()
    };
    store.update_lid(status.clone()).await;
    loop {
        tokio::select! {
            result = actions.join_next(), if !actions.is_empty() => {
                match result.context("lid action worker ended")? {
                    Ok(Ok(Some(state))) => { retry.clear(); store.update_power_sleep(state).await; status.error = None; }
                    Ok(Ok(None)) => { retry.clear(); status.error = None; }
                    Ok(Err(error)) => {
                        let pending = retry.failed(&error);
                        status.error = Some(format!("Lid action failed{}: {error:#}",
                            if pending { "; retrying preparation while lid remains closed" } else { "" }));
                    }
                    Err(error) => { retry.clear(); status.error = Some(format!("Lid action worker failed: {error}")); },
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
        if !closed || !managed || docked || retry.policy.as_ref().is_some_and(|p| p != &policy) {
            retry.clear();
        }
        if triggered && actions.is_empty() {
            retry.start(policy.clone());
            actions.spawn(perform(policy, power_sleep::operation_id()));
        } else if actions.is_empty() && retry.due(power_sleep::operation_id()) {
            actions.spawn(perform(policy, retry.operation));
        }
        status.managed = managed;
        store.update_lid(status.clone()).await;
    }
}

/// One retry chain belongs to one observed close, not merely a closed lid.
/// Back off indefinitely for a slow locker/helper, but never replay a request
/// after dispatch or after another operation has superseded this episode.
#[derive(Default)]
struct LidRetry {
    policy: Option<SleepPolicy>,
    deadline: Option<tokio::time::Instant>,
    operation: u64,
    attempts: u32,
}
impl LidRetry {
    fn clear(&mut self) {
        *self = Self::default();
    }
    fn start(&mut self, policy: SleepPolicy) {
        self.clear();
        self.policy = Some(policy);
    }
    fn failed(&mut self, error: &anyhow::Error) -> bool {
        if error.is::<power_sleep::RetryCancelled>() {
            self.clear();
            return false;
        }
        let Some(proof) = error.downcast_ref::<power_sleep::RetryablePreflight>() else {
            self.clear();
            return false;
        };
        if self.policy.is_none() {
            return false;
        }
        self.operation = proof.0;
        self.attempts = self.attempts.saturating_add(1);
        let seconds = (2u64 << self.attempts.min(4)).min(30);
        self.deadline = Some(tokio::time::Instant::now() + Duration::from_secs(seconds));
        true
    }
    fn due(&mut self, operation: u64) -> bool {
        if self.deadline.is_some() && operation != self.operation {
            self.clear();
        }
        if self
            .deadline
            .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
        {
            self.deadline = None;
            return true;
        }
        false
    }
}

// The action owns its policy guard and connection; the observer must remain
// independent so session changes can release native lid ownership promptly.
async fn perform(
    policy: SleepPolicy,
    operation: u64,
) -> Result<Option<crate::model::PowerSleepState>> {
    let retryable =
        |error: anyhow::Error| error.context(power_sleep::RetryablePreflight(operation));
    let _guard = tokio::time::timeout(Duration::from_secs(3), POLICY_WRITE.lock())
        .await
        .context("lid policy is busy")
        .map_err(retryable)?;
    anyhow::ensure!(load().await? == policy, "lid policy changed before action");
    anyhow::ensure!(
        power_sleep::operation_id() == operation,
        "another sleep operation superseded this lid close"
    );
    let connection = power_sleep::system_bus().await.map_err(retryable)?;
    let (manager, session) = async {
        Ok::<_, anyhow::Error>((
            power_sleep::manager(&connection).await?,
            power_sleep::current_session(&connection).await?,
        ))
    }
    .await
    .map_err(retryable)?;
    act(&policy, &manager, &session, operation).await
}

pub(super) async fn active_local_graphical(session: &zbus::Proxy<'_>) -> Result<bool> {
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
        return Err(power_sleep::cancel_retry(anyhow::anyhow!(
            "lid reopened, dock connected, or graphical session became inactive"
        )));
    }
    Ok(())
}

async fn act(
    policy: &SleepPolicy,
    manager: &zbus::Proxy<'_>,
    session: &zbus::Proxy<'_>,
    operation: u64,
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
            power_sleep::perform_with_validation(
                action,
                || std::future::ready(Ok(())),
                || async {
                    confirm_operation(operation)?;
                    confirm_trigger(manager, session).await
                },
            )
            .await?
        }
        LidAction::Profile => {
            let profile = policy.profile(plugged().await?);
            perform_profile(profile, || async {
                confirm_operation(operation)?;
                let current = load().await?;
                validate_lid_selection(profile.hibernate_minutes, &current, plugged().await?)?;
                confirm_trigger(manager, session).await
            })
            .await?
        }
    };
    Ok(Some(state))
}

fn confirm_operation(previous: u64) -> Result<()> {
    if power_sleep::operation_id() != previous.saturating_add(1) {
        return Err(power_sleep::cancel_retry(anyhow::anyhow!(
            "another sleep operation superseded this lid close"
        )));
    }
    Ok(())
}

fn validate_lid_selection(selected_delay: u32, policy: &SleepPolicy, plugged: bool) -> Result<()> {
    if policy.lid_action != LidAction::Profile
        || policy.profile(plugged).hibernate_minutes != selected_delay
    {
        return Err(power_sleep::cancel_retry(anyhow::anyhow!(
            "lid power profile changed while preparing sleep; close the lid again to use the new profile"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LidAction, LidEdge, SleepPolicy, validate_lid_selection};

    #[tokio::test(start_paused = true)]
    async fn retries_only_proven_preflight_failures_in_the_same_episode() {
        let mut retry = super::LidRetry::default();
        retry.start(SleepPolicy::default());
        let error = anyhow::anyhow!("slow locker").context(crate::sleep::RetryablePreflight(7));
        assert!(retry.failed(&error));
        assert!(!retry.due(7));
        tokio::time::advance(std::time::Duration::from_secs(4)).await;
        assert!(retry.due(7));
        assert!(!retry.due(7));
        assert!(retry.failed(&error));
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        assert!(!retry.due(8)); // another operation superseded the close
        assert!(retry.policy.is_none());
        retry.start(SleepPolicy::default());
        assert!(!retry.failed(&anyhow::anyhow!("dispatch timed out")));
        assert!(retry.policy.is_none());
        retry.start(SleepPolicy::default());
        assert!(!retry.failed(&crate::sleep::cancel_retry(error)));
        let error = anyhow::anyhow!("busy").context(crate::sleep::RetryablePreflight(7));
        retry.start(SleepPolicy::default());
        retry.clear(); // reopened, docked, inactive, or policy changed
        assert!(!retry.failed(&error));
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
    fn lid_profile_changes_cancel_but_equivalent_profiles_remain_valid() {
        let mut policy = SleepPolicy {
            critical_battery: Default::default(),
            lid_action: LidAction::Profile,
            same_profile: false,
            battery: super::super::SleepProfile {
                sleep_minutes: 0,
                hibernate_minutes: 15,
            },
            plugged: super::super::SleepProfile {
                sleep_minutes: 60,
                hibernate_minutes: 120,
            },
        };
        assert!(serde_json::from_str::<LidAction>("\"shutdown\"").is_err());
        assert!(validate_lid_selection(120, &policy, true).is_ok());
        policy.plugged.sleep_minutes = 0;
        assert!(validate_lid_selection(120, &policy, true).is_ok());
        assert!(validate_lid_selection(15, &policy, false).is_ok());
        assert!(policy.idle_profile(false, 0).is_err());
        assert!(validate_lid_selection(120, &policy, false).is_err());
        assert!(validate_lid_selection(15, &policy, true).is_err());
        policy.same_profile = true;
        assert!(validate_lid_selection(15, &policy, true).is_ok());
        policy.same_profile = false;
        policy.plugged.hibernate_minutes = 15;
        assert!(validate_lid_selection(15, &policy, true).is_ok());
        policy.lid_action = LidAction::System;
        assert!(validate_lid_selection(15, &policy, true).is_err());
    }
}
