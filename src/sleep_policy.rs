//! Persisted idle profiles. Hypridle owns inactivity; systemd owns time asleep.
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use shelllist_daemon_core::XdgRoot;
use tokio::sync::Mutex;

use crate::{
    paths::{data_file, load_json_or_default, save_json_atomic},
    state::StateStore,
};

pub(crate) mod critical;
mod hypridle;
pub(crate) mod lid;
pub(crate) mod runtime;
pub(crate) use hypridle::run;

// Serializes saves, power-source changes and idle callbacks.
static POLICY_WRITE: Mutex<()> = Mutex::const_new(());
const MAX_MINUTES: u32 = 7 * 24 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SleepProfile {
    /// Zero disables automatic sleep.
    pub sleep_minutes: u32,
    /// Additional time asleep, not another inactivity deadline. Zero means suspend only.
    pub hibernate_minutes: u32,
}

impl Default for SleepProfile {
    fn default() -> Self {
        Self {
            sleep_minutes: 30,
            hibernate_minutes: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SleepPolicy {
    /// Older policies retain logind's declarative lid behavior.
    #[serde(default)]
    pub lid_action: lid::LidAction,
    #[serde(default)]
    pub critical_battery: critical::Policy,
    pub same_profile: bool,
    pub battery: SleepProfile,
    pub plugged: SleepProfile,
}

impl Default for SleepPolicy {
    fn default() -> Self {
        Self {
            lid_action: lid::LidAction::System,
            critical_battery: critical::Policy::default(),
            same_profile: true,
            battery: SleepProfile::default(),
            plugged: SleepProfile::default(),
        }
    }
}

impl SleepPolicy {
    pub(crate) fn validate(&self) -> Result<()> {
        self.critical_battery.validate()?;
        for profile in [&self.battery, &self.plugged] {
            if profile.sleep_minutes > MAX_MINUTES || profile.hibernate_minutes > MAX_MINUTES {
                bail!("sleep and hibernate delays must be 0 (Never) or 1–10080 minutes");
            }
        }
        Ok(())
    }

    pub(crate) fn profile(&self, plugged: bool) -> &SleepProfile {
        if self.same_profile || !plugged {
            &self.battery
        } else {
            &self.plugged
        }
    }

    fn uses_timed_hibernation(&self) -> bool {
        [self.profile(false), self.profile(true)].iter().any(|p| {
            p.hibernate_minutes > 0
                && (p.sleep_minutes > 0 || self.lid_action == lid::LidAction::Profile)
        })
    }

    fn idle_profile(&self, plugged: bool, expected_minutes: u32) -> Result<&SleepProfile> {
        let profile = self.profile(plugged);
        if profile.sleep_minutes == 0 || profile.sleep_minutes != expected_minutes {
            bail!("idle sleep profile changed; waiting for the new inactivity deadline");
        }
        Ok(profile)
    }

    fn profile_name(&self, plugged: bool) -> &'static str {
        if self.same_profile {
            "shared"
        } else if plugged {
            "plugged"
        } else {
            "battery"
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SleepPolicyState {
    pub available: bool,
    pub policy: SleepPolicy,
    pub active_profile: String,
    pub hibernate_available: bool,
    /// Supported and currently authorized/uninhibited, including helper policy.
    #[serde(default)]
    pub hibernate_ready: bool,
    pub hibernate_error: Option<String>,
    pub lid: lid::LidState,
    #[serde(default)]
    pub critical_battery: critical::State,
    pub last_error: Option<String>,
    pub error: Option<String>,
}

fn policy_path() -> PathBuf {
    data_file(XdgRoot::Config, "sleep.json")
}

async fn load() -> Result<SleepPolicy> {
    let policy: SleepPolicy = load_json_or_default(&policy_path(), "sleep policy").await?;
    policy.validate()?;
    Ok(policy)
}

fn base_config() -> Result<PathBuf> {
    std::env::var_os("BAR_DAEMON_IDLE_CONFIG")
        .map(PathBuf::from)
        .context("Enable programs.shelllist.sleep.enable in Home Manager to manage automatic sleep")
}

pub(crate) async fn plugged() -> Result<bool> {
    let connection = crate::sleep::system_bus().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.UPower",
        "/org/freedesktop/UPower",
        "org.freedesktop.UPower",
    )
    .await?;
    let on_battery: bool = proxy
        .get_property("OnBattery")
        .await
        .context("read AC power state")?;
    Ok(!on_battery)
}

async fn validate_config(policy: &SleepPolicy) -> Result<()> {
    let path = base_config()?;
    let base = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    // Check both profiles before persisting, including the inactive one.
    let executable = std::env::current_exe()?;
    for profile in [&policy.battery, &policy.plugged] {
        hypridle::render(&base, profile, &executable, "validate")?;
    }
    Ok(())
}

async fn user_systemd() -> Result<zbus::Connection> {
    crate::sleep::bounded("connect to user systemd", Duration::from_secs(3), async {
        Ok(zbus::connection::Builder::session()?
            .method_timeout(Duration::from_secs(3))
            .build()
            .await?)
    })
    .await
}

async fn apply_idle() -> Result<()> {
    idle_running().await?;
    let policy = load().await?;
    hypridle::set_timeout(policy.profile(plugged().await?).sleep_minutes).await?;
    idle_running().await
}

async fn idle_running() -> Result<()> {
    let connection = user_systemd().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1/unit/hypridle_2eservice",
        "org.freedesktop.systemd1.Unit",
    )
    .await?;
    let active: String = proxy
        .get_property("ActiveState")
        .await
        .context("read hypridle service state")?;
    let substate: String = proxy.get_property("SubState").await?;
    let service = zbus::Proxy::new(
        &connection,
        "org.freedesktop.systemd1",
        proxy.path().clone(),
        "org.freedesktop.systemd1.Service",
    )
    .await?;
    let kind: String = service.get_property("Type").await?;
    let pid: u32 = service.get_property("MainPID").await?;
    verify_idle_readiness(
        &active,
        &substate,
        &kind,
        pid,
        &hypridle::active_generation().await?,
    )
}

fn verify_idle_readiness(
    active: &str,
    substate: &str,
    kind: &str,
    pid: u32,
    generation: &str,
) -> Result<()> {
    if kind != "notify" {
        bail!(
            "hypridle readiness integration is missing; activate the updated Shelllist Home Manager module"
        );
    }
    if active != "active" || substate != "running" || pid == 0 {
        bail!("hypridle is not ready ({active}/{substate})");
    }
    let generated_pid = generation
        .split('-')
        .next()
        .and_then(|part| part.parse::<u32>().ok());
    if generated_pid != Some(pid) {
        bail!("generated idle configuration belongs to a different hypridle process");
    }
    Ok(())
}

pub(crate) async fn set(policy: SleepPolicy, store: &StateStore) -> Result<SleepPolicyState> {
    let _guard = POLICY_WRITE.lock().await;
    policy.validate()?;
    validate_config(&policy).await?;
    idle_running().await?;
    let plugged = plugged().await?;
    // A missing helper/capability must not be presented as working hibernation.
    let support = hibernate_support().await;
    let timed_hibernation = policy.uses_timed_hibernation();
    if timed_hibernation {
        if let Err(error) = &support {
            bail!("{error:#}");
        }
    }
    let previous = load().await?;
    if policy.critical_battery.enabled && !previous.critical_battery.enabled {
        let connection = crate::sleep::system_bus().await?;
        let capability: String = crate::sleep::manager(&connection)
            .await?
            .call("CanHibernate", &())
            .await?;
        anyhow::ensure!(
            crate::sleep::capability::configurable(&capability),
            "critical battery protection requires hibernation support: {capability}"
        );
    }
    persist_and_apply(&policy_path(), &policy, &previous, apply_idle).await?;
    if !timed_hibernation {
        // Disabling protection must remain possible with an unavailable helper.
        let cleanup: Result<()> = async {
            let connection = crate::sleep::system_bus().await?;
            let _: bool = helper_proxy(&connection)
                .await?
                .call("CleanupHibernateDelay", &())
                .await?;
            Ok(())
        }
        .await;
        if let Err(error) = cleanup {
            tracing::warn!(%error, "runtime hibernate cleanup deferred to helper maintenance");
        }
    }
    let state = SleepPolicyState {
        available: true,
        active_profile: policy.profile_name(plugged).into(),
        policy,
        hibernate_available: support.is_ok(),
        hibernate_ready: matches!(&support, Ok(None)),
        hibernate_error: support.unwrap_or_else(|error| Some(format!("{error:#}"))),
        ..Default::default()
    };
    store.update_sleep_policy(state).await;
    Ok(store.read(|s| s.sleep_policy.clone()).await)
}

async fn hibernate_support() -> Result<Option<String>> {
    let capability = crate::sleep::combined_capability().await?;
    anyhow::ensure!(
        crate::sleep::capability::configurable(&capability),
        "suspend-then-hibernate cannot be configured: {capability}"
    );
    let helper_authorized = helper_available().await?;
    Ok(if !helper_authorized {
        Some("Hibernate delay helper requires system-policy authorization; automatic sleep cannot prompt.".into())
    } else {
        crate::sleep::capability::limitation(&capability).map(str::to_owned)
    })
}

async fn persist_and_apply<F, Fut>(
    path: &Path,
    policy: &SleepPolicy,
    previous: &SleepPolicy,
    mut apply: F,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    save_json_atomic(path, policy).await?;
    if let Err(error) = apply().await {
        save_json_atomic(path, previous)
            .await
            .context("restore previous sleep policy after live timeout update failed")?;
        apply().await.with_context(|| format!("new sleep policy failed ({error:#}); previous policy was restored but its live timeout could not be confirmed"))?;
        return Err(error).context("sleep settings not applied; previous policy restored");
    }
    Ok(())
}

async fn helper_available() -> Result<bool> {
    let connection = crate::sleep::system_bus().await?;
    let proxy = helper_proxy(&connection).await?;
    let available: bool = proxy
        .call("SleepSettingsAvailable", &())
        .await
        .context("Install the updated system bar-battery-helper to configure hibernation")?;
    if !available {
        bail!("system hibernate settings are unavailable");
    }
    proxy
        .call("CanSetHibernateDelay", &())
        .await
        .context("Install the updated helper authorization probe")
}

async fn helper_proxy(connection: &zbus::Connection) -> Result<zbus::Proxy<'_>> {
    zbus::Proxy::new(
        connection,
        crate::battery::helper::BUS_NAME,
        crate::battery::helper::OBJECT_PATH,
        crate::battery::helper::INTERFACE,
    )
    .await
    .map_err(Into::into)
}

/// Called only by the generated hypridle listener. Re-check the power source at
/// the deadline so an AC change cannot trigger the previous profile early.
pub(crate) async fn idle_sleep(
    expected_minutes: u32,
    generation: &str,
    episode: u64,
    store: &StateStore,
) -> Result<crate::model::PowerSleepState> {
    let _guard = tokio::time::timeout(Duration::from_secs(3), POLICY_WRITE.lock())
        .await
        .context("idle policy is busy; this callback was cancelled")?;
    let result = perform_idle(expected_minutes, generation, episode).await;
    let mut state = store.read(|s| s.sleep_policy.clone()).await;
    state.last_error = result.as_ref().err().map(|error| format!("{error:#}"));
    store.update_sleep_policy(state).await;
    result
}

async fn perform_idle(
    expected_minutes: u32,
    generation: &str,
    episode: u64,
) -> Result<crate::model::PowerSleepState> {
    base_config()?;
    hypridle::verify_episode(generation, episode).await?;
    let policy = load().await?;
    let profile = policy.idle_profile(plugged().await?, expected_minutes)?;
    perform_profile(profile, || {
        validate_idle_trigger(generation, episode, profile)
    })
    .await
}

async fn validate_idle_trigger(
    generation: &str,
    episode: u64,
    selected: &SleepProfile,
) -> Result<()> {
    base_config()?;
    let policy = load().await?;
    let plugged = plugged().await?;
    // Check the generation after the potentially slow AC lookup as well as at
    // entry. An external hypridle restart is not serialized by POLICY_WRITE.
    hypridle::verify_episode(generation, episode).await?;
    validate_idle_selection(selected, &policy, plugged)
}

fn validate_idle_selection(
    selected: &SleepProfile,
    policy: &SleepPolicy,
    plugged: bool,
) -> Result<()> {
    let current = policy.idle_profile(plugged, selected.sleep_minutes)?;
    if current != selected {
        bail!("idle hibernate profile changed; refusing to use the previous sleep settings");
    }
    Ok(())
}

/// Shared by idle and lid-close. A lid action can use the hibernate delay even
/// when automatic inactivity sleep is Never. Revalidate the trigger after lock
/// confirmation and privileged setup, immediately before logind preflight.
async fn perform_profile<F, Fut>(
    profile: &SleepProfile,
    validate_trigger: F,
) -> Result<crate::model::PowerSleepState>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let action = if profile.hibernate_minutes == 0 {
        "suspend"
    } else {
        "suspend-then-hibernate"
    };
    crate::sleep::perform_with_validation(
        action,
        || {
            validated_setup(&validate_trigger, || async {
                if profile.hibernate_minutes > 0 {
                    let connection = crate::sleep::system_bus().await?;
                    let proxy = helper_proxy(&connection).await?;
                    let fd: zvariant::OwnedFd = proxy
                        .call("AcquireHibernateDelay", &(profile.hibernate_minutes,))
                        .await
                        .context("lease and verify systemd's time asleep before hibernation")?;
                    crate::sleep::retain_lease(fd)?;
                }
                Ok(())
            })
        },
        &validate_trigger,
    )
    .await
}

// Runs only after confirmed locking. A changed trigger must prevent helper
// writes, and a change while the helper is running must prevent sleep.
async fn validated_setup<F, Fut, S, Setup>(validate: F, setup: S) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
    S: FnOnce() -> Setup,
    Setup: std::future::Future<Output = Result<()>>,
{
    validate().await?;
    setup().await?;
    validate().await
}

pub(crate) async fn monitor(store: StateStore) {
    let mut events = store.subscribe();
    let mut timer = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = timer.tick() => {},
            event = events.recv() => {
                match event {
                    Ok(event) if event.stream == crate::protocol::stream::BATTERY => {},
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    _ => continue,
                }
            }
        }
        let _guard = POLICY_WRITE.lock().await;
        let mut state = SleepPolicyState {
            last_error: store.read(|s| s.sleep_policy.last_error.clone()).await,
            ..SleepPolicyState::default()
        };
        let result: Result<()> = async {
            state.policy = load().await?;
            validate_config(&state.policy).await?;
            idle_running().await?;
            let plugged = plugged().await?;
            state.active_profile = state.policy.profile_name(plugged).into();
            let profile = state.policy.profile(plugged);
            if hypridle::active_minutes().await? != profile.sleep_minutes {
                apply_idle().await?;
            }
            let support = hibernate_support().await;
            state.hibernate_available = support.is_ok();
            state.hibernate_ready = matches!(&support, Ok(None));
            state.hibernate_error = support.unwrap_or_else(|error| Some(format!("{error:#}")));
            state.available = true;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            state.error = Some(format!("{error:#}"));
        }
        store.update_sleep_policy(state).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_and_separate_profiles_preserve_both_values() {
        let mut policy = SleepPolicy {
            critical_battery: Default::default(),
            lid_action: lid::LidAction::System,
            same_profile: false,
            battery: SleepProfile {
                sleep_minutes: 10,
                hibernate_minutes: 60,
            },
            plugged: SleepProfile {
                sleep_minutes: 45,
                hibernate_minutes: 180,
            },
        };
        assert_eq!(policy.profile(false).sleep_minutes, 10);
        assert_eq!(policy.profile(true).sleep_minutes, 45);
        policy.same_profile = true;
        assert_eq!(policy.profile(true).sleep_minutes, 10);
        assert_eq!(policy.plugged.hibernate_minutes, 180);
        assert!(policy.validate().is_ok());
        policy.plugged.hibernate_minutes = MAX_MINUTES + 1;
        assert!(
            policy.validate().is_err(),
            "even hidden profiles are validated"
        );
        assert!(
            serde_json::from_str::<SleepProfile>(r#"{"sleep_minutes":-1,"hibernate_minutes":0}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<SleepProfile>(r#"{"sleep_minutes":1.5,"hibernate_minutes":0}"#)
                .is_err()
        );
    }

    #[test]
    fn stale_idle_callbacks_cannot_sleep_early_after_ac_changes_or_never() {
        let policy = SleepPolicy {
            critical_battery: Default::default(),
            lid_action: lid::LidAction::System,
            same_profile: false,
            battery: SleepProfile {
                sleep_minutes: 10,
                hibernate_minutes: 60,
            },
            plugged: SleepProfile {
                sleep_minutes: 45,
                hibernate_minutes: 180,
            },
        };
        assert!(policy.idle_profile(true, 10).is_err());
        assert!(policy.idle_profile(false, 45).is_err());
        assert_eq!(
            policy.idle_profile(true, 45).unwrap().hibernate_minutes,
            180
        );
        let never = SleepPolicy {
            battery: SleepProfile {
                sleep_minutes: 0,
                hibernate_minutes: 60,
            },
            ..Default::default()
        };
        assert!(never.idle_profile(false, 0).is_err());
        assert!(never.idle_profile(false, 10).is_err());
    }

    #[tokio::test]
    async fn idle_trigger_changes_after_lock_or_during_helper_abort_sleep_setup() {
        use std::cell::Cell;

        for change in [
            "longer-ac-deadline",
            "never-on-ac",
            "different-hibernate-delay",
            "restarted",
        ] {
            for during_helper in [false, true] {
                let policy = SleepPolicy {
                    same_profile: false,
                    battery: SleepProfile {
                        sleep_minutes: 10,
                        hibernate_minutes: 60,
                    },
                    plugged: match change {
                        "longer-ac-deadline" => SleepProfile {
                            sleep_minutes: 45,
                            hibernate_minutes: 60,
                        },
                        "never-on-ac" => SleepProfile {
                            sleep_minutes: 0,
                            hibernate_minutes: 60,
                        },
                        _ => SleepProfile {
                            sleep_minutes: 10,
                            hibernate_minutes: 180,
                        },
                    },
                    ..SleepPolicy::default()
                };
                let changed = Cell::new(!during_helper);
                let helper_calls = Cell::new(0);
                let result = validated_setup(
                    || {
                        std::future::ready(if change == "restarted" {
                            hypridle::validate_episode(
                                "original",
                                1,
                                if changed.get() {
                                    "replacement"
                                } else {
                                    "original"
                                },
                                1,
                                true,
                            )
                        } else {
                            validate_idle_selection(&policy.battery, &policy, changed.get())
                        })
                    },
                    || async {
                        helper_calls.set(helper_calls.get() + 1);
                        changed.set(true);
                        Ok(())
                    },
                )
                .await;
                assert!(result.is_err(), "{change}, during_helper={during_helper}");
                assert_eq!(
                    helper_calls.get(),
                    usize::from(during_helper),
                    "a change while locking must not write hibernate settings"
                );
            }
        }
    }

    #[test]
    fn unchanged_and_equivalent_shared_profiles_remain_valid() {
        let policy = SleepPolicy::default();
        for plugged in [false, true] {
            assert!(validate_idle_selection(&policy.battery, &policy, plugged).is_ok());
        }
        let mut separate = policy.clone();
        separate.same_profile = false;
        assert!(validate_idle_selection(&policy.battery, &separate, true).is_ok());
    }

    #[tokio::test]
    async fn saves_durably_and_rolls_back_if_live_idle_update_fails() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sleep.json");
        let previous = SleepPolicy::default();
        let next = SleepPolicy {
            same_profile: false,
            ..Default::default()
        };
        persist_and_apply(&path, &next, &previous, || async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(
            load_json_or_default::<SleepPolicy>(&path, "test")
                .await
                .unwrap(),
            next
        );
        let mut calls = 0;
        let error = persist_and_apply(&path, &next, &previous, || {
            calls += 1;
            std::future::ready(if calls == 1 {
                Err(anyhow::anyhow!("live update failed"))
            } else {
                Ok(())
            })
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("previous policy restored"));
        assert_eq!(
            calls, 2,
            "rollback also restores the old live timeout without restarting Hypridle"
        );
        assert_eq!(
            load_json_or_default::<SleepPolicy>(&path, "test")
                .await
                .unwrap(),
            previous
        );
    }

    #[test]
    fn generated_config_alone_never_proves_idle_readiness() {
        assert!(verify_idle_readiness("active", "running", "simple", 42, "42-123").is_err());
        assert!(verify_idle_readiness("activating", "start", "notify", 42, "42-123").is_err());
        assert!(verify_idle_readiness("failed", "failed", "notify", 0, "42-123").is_err());
        assert!(verify_idle_readiness("active", "running", "notify", 43, "42-123").is_err());
        assert!(verify_idle_readiness("active", "running", "notify", 42, "42-123").is_ok());
    }
}
