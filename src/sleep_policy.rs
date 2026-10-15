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
    // The general idle/lid API does not own critical-battery settings. Clients
    // may omit this field (serde defaults) or submit an older full snapshot.
    fn with_stored_critical(mut self, previous: &Self) -> Self {
        self.critical_battery = previous.critical_battery;
        self
    }

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

/// Recovery is allowed only for an explicit settings write. Monitors/actions
/// still use strict load() and never silently act on defaults after corruption.
async fn load_for_update(path: &Path) -> Result<SleepPolicy> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SleepPolicy::default());
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let parsed = serde_json::from_slice::<SleepPolicy>(&bytes)
        .context("parse sleep policy")
        .and_then(|policy| {
            policy.validate()?;
            Ok(policy)
        });
    match parsed {
        Ok(policy) => Ok(policy),
        Err(error) => {
            let parent = path
                .parent()
                .context("sleep policy has no parent directory")?
                .to_owned();
            let backup = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
                use std::io::Write;
                let mut backup = tempfile::Builder::new()
                    .prefix("sleep.json.invalid-")
                    .tempfile_in(&parent)?;
                backup.write_all(&bytes)?;
                backup.as_file().sync_all()?;
                let (_, path) = backup.keep()?;
                std::fs::File::open(parent)?.sync_all()?;
                Ok(path)
            })
            .await
            .context("back up invalid sleep policy")??;
            tracing::warn!(%error, backup = %backup.display(), "invalid sleep policy backed up; explicit settings write will recover from defaults");
            Ok(SleepPolicy::default())
        }
    }
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
    crate::bus::session().await
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
    let previous = load_for_update(&policy_path()).await?;
    let policy = policy.with_stored_critical(&previous);
    policy.validate()?;
    validate_config(&policy).await?;
    idle_running().await?;
    let plugged = plugged().await?;
    // A missing helper/capability must not be presented as working hibernation.
    let support = hibernate_support().await;
    let timed_hibernation = policy.uses_timed_hibernation();
    if timed_hibernation && let Err(error) = &support {
        bail!("{error:#}");
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
    let result = async {
        // Keep our place in the mutex queue, but abandon obsolete idle episodes
        // promptly. Contention alone must not consume hypridle's only callback.
        let acquire = POLICY_WRITE.lock();
        tokio::pin!(acquire);
        let _guard = loop {
            tokio::select! {
                guard = &mut acquire => break guard,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    hypridle::verify_episode(generation, episode).await?;
                }
            }
        };
        perform_idle(expected_minutes, generation, episode).await
    }
    .await;
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
    let mut events = store.subscribe_domains(&[crate::protocol::stream::BATTERY]);
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
        let ((mut state, observed, update), _guard) = observe_then_lock(observe_policy()).await;
        // A setter may have committed while observations were in flight. Never
        // apply or publish that old snapshot over the newly saved policy.
        if load().await.ok() != observed {
            timer.reset_immediately();
            continue;
        }
        if let Some(minutes) = update
            && let Err(error) = hypridle::set_timeout(minutes).await
        {
            state.available = false;
            state.error = Some(format!("{error:#}"));
        }
        state.last_error = store.read(|s| s.sleep_policy.last_error.clone()).await;
        store.update_sleep_policy(state).await;
    }
}

// Only comparison, live timeout mutation and publication need serialization.
// In particular, never hold POLICY_WRITE during capability/polkit probes.
async fn observe_then_lock<T>(
    observation: impl std::future::Future<Output = T>,
) -> (T, tokio::sync::MutexGuard<'static, ()>) {
    let observed = observation.await;
    (observed, POLICY_WRITE.lock().await)
}

async fn observe_policy() -> (SleepPolicyState, Option<SleepPolicy>, Option<u32>) {
    let mut state = SleepPolicyState::default();
    let mut observed = None;
    let mut update = None;
    let result: Result<()> = async {
        state.policy = load().await?;
        observed = Some(state.policy.clone());
        validate_config(&state.policy).await?;
        idle_running().await?;
        let active_minutes = hypridle::active_minutes().await?;
        let support = hibernate_support().await;
        state.hibernate_available = support.is_ok();
        state.hibernate_ready = matches!(&support, Ok(None));
        state.hibernate_error = support.unwrap_or_else(|error| Some(format!("{error:#}")));
        // Read power last, after potentially slow authorization, to minimize
        // staleness. Idle actions independently revalidate AC before dispatch.
        let plugged = plugged().await?;
        state.active_profile = state.policy.profile_name(plugged).into();
        let minutes = state.policy.profile(plugged).sleep_minutes;
        update = (active_minutes != minutes).then_some(minutes);
        state.available = true;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        state.error = Some(format!("{error:#}"));
    }
    (state, observed, update)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn explicit_writes_recover_bad_policy_with_backup_but_not_io_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sleep.json");
        assert_eq!(
            load_for_update(&path).await.unwrap(),
            SleepPolicy::default()
        );
        for bytes in [
            b"{broken".to_vec(),
            serde_json::to_vec(&SleepPolicy {
                battery: SleepProfile {
                    sleep_minutes: MAX_MINUTES + 1,
                    hibernate_minutes: 0,
                },
                ..Default::default()
            })
            .unwrap(),
        ] {
            tokio::fs::write(&path, &bytes).await.unwrap();
            assert_eq!(
                load_for_update(&path).await.unwrap(),
                SleepPolicy::default()
            );
            assert_eq!(
                tokio::fs::read(&path).await.unwrap(),
                bytes,
                "recovery load must not overwrite the original"
            );
            assert!(
                std::fs::read_dir(dir.path())
                    .unwrap()
                    .flatten()
                    .any(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with("sleep.json.invalid-")
                            && std::fs::read(entry.path()).unwrap() == bytes
                    })
            );
        }
        let valid = SleepPolicy {
            lid_action: lid::LidAction::Lock,
            ..Default::default()
        };
        save_json_atomic(&path, &valid).await.unwrap();
        assert_eq!(load_for_update(&path).await.unwrap(), valid);
        assert!(
            load_for_update(dir.path()).await.is_err(),
            "I/O failures are not corruption"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn general_policy_updates_cannot_enable_disable_or_overwrite_critical_settings() {
        for enabled in [false, true] {
            let previous = SleepPolicy {
                critical_battery: critical::Policy {
                    enabled,
                    percent: 7,
                    grace_seconds: 90,
                },
                ..Default::default()
            };
            let requested = SleepPolicy {
                critical_battery: critical::Policy {
                    enabled: !enabled,
                    percent: 2,
                    grace_seconds: 30,
                },
                lid_action: lid::LidAction::Suspend,
                battery: SleepProfile {
                    sleep_minutes: 12,
                    hibernate_minutes: 0,
                },
                ..Default::default()
            };
            let effective = requested.clone().with_stored_critical(&previous);
            assert_eq!(effective.critical_battery, previous.critical_battery);
            assert_eq!(effective.lid_action, requested.lid_action);
            assert_eq!(effective.battery, requested.battery);
            let mut omitted = serde_json::to_value(&requested).unwrap();
            omitted.as_object_mut().unwrap().remove("critical_battery");
            let omitted: SleepPolicy = serde_json::from_value(omitted).unwrap();
            assert_eq!(
                omitted.with_stored_critical(&previous).critical_battery,
                previous.critical_battery
            );
        }
    }

    #[tokio::test]
    async fn idle_trigger_changes_after_lock_or_during_helper_abort_sleep_setup() {
        for (active, substate, kind, pid, ready) in [
            ("active", "running", "simple", 42, false),
            ("activating", "start", "notify", 42, false),
            ("failed", "failed", "notify", 0, false),
            ("active", "running", "notify", 43, false),
            ("active", "running", "notify", 42, true),
        ] {
            assert_eq!(
                verify_idle_readiness(active, substate, kind, pid, "42-123").is_ok(),
                ready
            );
        }
        use std::cell::Cell;

        for change in [
            "longer-ac-deadline",
            "never-on-ac",
            "different-hibernate-delay",
            "restarted",
            "unchanged",
            "shared",
            "equivalent",
        ] {
            for during_helper in [false, true] {
                let allowed = matches!(change, "unchanged" | "shared" | "equivalent");
                let policy = SleepPolicy {
                    same_profile: change == "shared",
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
                        "unchanged" | "equivalent" => SleepProfile {
                            sleep_minutes: 10,
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
                            validate_idle_selection(
                                &policy.battery,
                                &policy,
                                changed.get() && change != "unchanged",
                            )
                        })
                    },
                    || async {
                        helper_calls.set(helper_calls.get() + 1);
                        changed.set(true);
                        Ok(())
                    },
                )
                .await;
                assert_eq!(
                    result.is_ok(),
                    allowed,
                    "{change}, during_helper={during_helper}"
                );
                assert_eq!(
                    helper_calls.get(),
                    usize::from(allowed || during_helper),
                    "a change while locking must not write hibernate settings"
                );
            }
        }
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
        assert!(next.validate().is_ok());
        let mut invalid = next.clone();
        invalid.same_profile = true;
        invalid.plugged.hibernate_minutes = MAX_MINUTES + 1;
        assert!(
            invalid.validate().is_err(),
            "hidden profiles must also be validated"
        );
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
}
