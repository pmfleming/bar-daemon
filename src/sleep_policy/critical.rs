//! Opt-in awake critical-battery protection. One cancellable hibernate attempt
//! per low-battery episode, never a shutdown fallback or inhibitor bypass.
use crate::{
    activity::notifications::{
        model::{IncomingNotification, NotificationHints},
        service::NotificationSink,
    },
    sleep,
    state::StateStore,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Policy {
    pub enabled: bool,
    pub percent: u8,
    pub grace_seconds: u32,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            percent: 5,
            grace_seconds: 60,
        }
    }
}
impl Policy {
    pub(super) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            (1..=20).contains(&self.percent) && (30..=300).contains(&self.grace_seconds),
            "critical battery requires 1–20 percent and a 30–300 second grace period"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct State {
    pub phase: String,
    pub remaining_seconds: u32,
    pub error: Option<String>,
}
static CANCELLATION: AtomicU64 = AtomicU64::new(0);
static RECOVERY_WRITE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Recovery {
    policy: Policy,
    attempted: bool,
}
fn recovery_path() -> std::path::PathBuf {
    crate::paths::data_file(
        shelllist_daemon_core::XdgRoot::State,
        "critical-battery.json",
    )
}
async fn persist_recovery(mut recovery: Recovery, token: u64) -> Result<Recovery> {
    let _guard = RECOVERY_WRITE.lock().await;
    if CANCELLATION.load(Ordering::SeqCst) != token {
        recovery.attempted = true;
    }
    crate::paths::save_json_atomic(&recovery_path(), &recovery).await?;
    Ok(recovery)
}

#[derive(Clone, Copy)]
struct Sample {
    percent: f64,
    on_battery: bool,
    discharging: bool,
}
impl Sample {
    fn critical(self, policy: &Policy) -> bool {
        self.on_battery && self.discharging && self.percent <= f64::from(policy.percent)
    }
    fn recovered(self, policy: &Policy) -> bool {
        !self.on_battery || self.percent >= f64::from(policy.percent.saturating_add(3))
    }
}
#[derive(Default)]
struct Engine {
    policy: Option<Policy>,
    deadline: Option<u64>,
    token: u64,
    latched: bool,
    state: State,
}
impl Engine {
    fn stop(&mut self, phase: &str, error: Option<String>) {
        self.deadline = None;
        self.latched = true;
        self.state = State {
            phase: phase.into(),
            error,
            remaining_seconds: 0,
        };
    }
    // Returns (new warning needed, request now). Pure time/input state machine.
    fn observe(
        &mut self,
        policy: &Policy,
        sample: Option<Sample>,
        now: u64,
        token: u64,
    ) -> (bool, bool) {
        if self.policy.as_ref() != Some(policy) {
            self.policy = Some(*policy);
            self.deadline = None;
            self.latched = false;
        }
        if !policy.enabled {
            self.state = State {
                phase: "disabled".into(),
                ..Default::default()
            };
            return (false, false);
        }
        let Some(sample) = sample else {
            self.deadline = None;
            self.state = State {
                phase: "blocked".into(),
                error: Some(
                    "Fresh battery evidence unavailable; automatic hibernation cancelled".into(),
                ),
                ..Default::default()
            };
            return (false, false);
        };
        if sample.recovered(policy) {
            self.latched = false;
            self.deadline = None;
            self.state = State {
                phase: "armed".into(),
                ..Default::default()
            };
            return (false, false);
        }
        if self.deadline.is_some() && token != self.token {
            self.stop("cancelled", None);
        }
        if self.latched {
            return (false, false);
        }
        if !sample.critical(policy) {
            self.deadline = None;
            self.state = State {
                phase: "armed".into(),
                ..Default::default()
            };
            return (false, false);
        }
        let warning = self.deadline.is_none();
        if warning {
            self.token = token;
        }
        let remaining = self
            .deadline
            .get_or_insert_with(|| now.saturating_add(u64::from(policy.grace_seconds)))
            .saturating_sub(now);
        if remaining == 0 {
            self.stop("acting", None);
            return (false, true);
        }
        self.state = State {
            phase: "countdown".into(),
            remaining_seconds: remaining as u32,
            error: None,
        };
        (warning, false)
    }
}

async fn sample() -> Result<Sample> {
    sleep::bounded(
        "read critical battery evidence",
        Duration::from_secs(3),
        async {
            let connection = sleep::system_bus().await?;
            let root = zbus::Proxy::new(
                &connection,
                "org.freedesktop.UPower",
                "/org/freedesktop/UPower",
                "org.freedesktop.UPower",
            )
            .await?;
            let path: zvariant::OwnedObjectPath = root.call("GetDisplayDevice", &()).await?;
            let device = zbus::Proxy::new(
                &connection,
                "org.freedesktop.UPower",
                path,
                "org.freedesktop.UPower.Device",
            )
            .await?;
            anyhow::ensure!(
                device.get_property::<bool>("IsPresent").await?,
                "no aggregate battery present"
            );
            let percent: f64 = device.get_property("Percentage").await?;
            anyhow::ensure!(
                percent.is_finite() && (0.0..=100.0).contains(&percent),
                "invalid battery percentage"
            );
            let warning: u32 = device.get_property("WarningLevel").await?;
            anyhow::ensure!(
                warning < 5,
                "UPower is already at its emergency-action level; not competing with it"
            );
            let state: u32 = device.get_property("State").await?;
            Ok(Sample {
                percent,
                on_battery: root.get_property("OnBattery").await?,
                discharging: state == 2,
            })
        },
    )
    .await
}
async fn no_other_power_manager() -> Result<()> {
    let connection = super::user_systemd().await?;
    let bus = zbus::Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await?;
    for name in [
        "org.kde.Solid.PowerManagement",
        "org.gnome.SettingsDaemon.Power",
        "org.xfce.PowerManager",
        "org.mate.PowerManager",
    ] {
        let owned: bool = bus.call("NameHasOwner", &(name,)).await?;
        anyhow::ensure!(
            !owned,
            "another desktop power manager ({name}) is active; critical protection is not taken over"
        );
    }
    Ok(())
}
async fn validate(policy: &Policy, token: u64) -> Result<()> {
    anyhow::ensure!(
        CANCELLATION.load(Ordering::SeqCst) == token,
        "critical hibernation was cancelled"
    );
    anyhow::ensure!(
        super::load().await?.critical_battery == *policy,
        "critical battery policy changed"
    );
    no_other_power_manager().await?;
    let connection = sleep::system_bus().await?;
    let session = sleep::current_session(&connection).await?;
    anyhow::ensure!(
        super::lid::active_local_graphical(&session).await?,
        "critical protection requires the active local graphical session"
    );
    anyhow::ensure!(
        sample().await?.critical(policy),
        "battery recovered or AC connected; critical hibernation cancelled"
    );
    anyhow::ensure!(
        CANCELLATION.load(Ordering::SeqCst) == token,
        "critical hibernation was cancelled"
    );
    Ok(())
}
async fn perform(policy: Policy, token: u64) -> Result<crate::model::PowerSleepState> {
    validate(&policy, token).await?;
    sleep::perform_with_validation(
        "hibernate",
        || validate(&policy, token),
        || validate(&policy, token),
    )
    .await
}
pub(crate) async fn set_policy(
    policy: Policy,
    store: &StateStore,
) -> Result<super::SleepPolicyState> {
    policy.validate()?;
    let _guard = tokio::time::timeout(Duration::from_secs(3), super::POLICY_WRITE.lock())
        .await
        .map_err(|_| anyhow::anyhow!("sleep policy is busy"))?;
    let mut previous = super::load().await?;
    if policy.enabled && !previous.critical_battery.enabled {
        no_other_power_manager().await?;
        let connection = sleep::system_bus().await?;
        let capability: String = sleep::manager(&connection)
            .await?
            .call("CanHibernate", &())
            .await?;
        anyhow::ensure!(
            sleep::capability::configurable(&capability),
            "critical battery protection requires hibernation support: {capability}"
        );
    }
    previous.critical_battery = policy;
    crate::paths::save_json_atomic(&super::policy_path(), &previous).await?;
    let mut state = store.read(|s| s.sleep_policy.clone()).await;
    state.policy = previous;
    store.update_sleep_policy(state).await;
    Ok(store.read(|s| s.sleep_policy.clone()).await)
}

pub(crate) async fn cancel(store: &StateStore) -> Result<State> {
    let phase = store
        .read(|s| s.sleep_policy.critical_battery.phase.clone())
        .await;
    if !matches!(phase.as_str(), "countdown" | "acting") {
        bail!("no critical battery countdown is pending");
    }
    let token = CANCELLATION.fetch_add(1, Ordering::SeqCst) + 1;
    let policy = super::load().await?.critical_battery;
    // Invalidate first, then persist: cancellation never waits for the policy
    // transaction mutex, and a daemon restart must not replay this episode.
    persist_recovery(
        Recovery {
            policy,
            attempted: true,
        },
        token,
    )
    .await?;
    let state = State {
        phase: "cancelled".into(),
        error: Some(
            "Cancellation requested. An already dispatched sleep request cannot be undone.".into(),
        ),
        ..Default::default()
    };
    store.update_critical_battery(state.clone()).await;
    Ok(state)
}

pub(crate) async fn monitor(store: StateStore, notifications: NotificationSink) {
    let mut saved: Recovery =
        match crate::paths::load_json_or_default(&recovery_path(), "critical battery recovery")
            .await
        {
            Ok(saved) => saved,
            Err(error) => {
                store
                    .update_critical_battery(State {
                        phase: "blocked".into(),
                        error: Some(format!(
                            "Cannot safely recover critical battery state: {error:#}"
                        )),
                        ..Default::default()
                    })
                    .await;
                return;
            }
        };
    let mut engine = Engine {
        policy: Some(saved.policy),
        latched: saved.attempted,
        state: State {
            phase: if saved.attempted {
                "latched"
            } else {
                "disabled"
            }
            .into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut actions = tokio::task::JoinSet::new();
    let mut events = store.subscribe();
    let mut timer = tokio::time::interval(Duration::from_secs(5));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let started = std::time::Instant::now();
    loop {
        tokio::select! {
            _ = timer.tick() => {},
            event = events.recv() => match event {
                Ok(e) if e.stream == crate::protocol::stream::BATTERY => {},
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                _ => continue,
            },
            result = actions.join_next(), if !actions.is_empty() => {
                match result {
                    Some(Ok(Ok(state))) => { store.update_power_sleep(state).await; engine.stop("accepted", None); }
                    Some(Ok(Err(error))) => engine.stop("failed", Some(format!("{error:#}"))),
                    _ => engine.stop("failed", Some("critical battery action worker stopped".into())),
                }
                store.update_critical_battery(engine.state.clone()).await; continue;
            }
        }
        match engine.refresh(&mut saved, &notifications, started).await {
            Ok(Some(policy)) if actions.is_empty() => {
                actions.spawn(perform(policy, engine.token));
            }
            Err(error) => engine.stop("blocked", Some(format!("{error:#}"))),
            _ => {}
        }
        store.update_critical_battery(engine.state.clone()).await;
    }
}

impl Engine {
    /// Read fresh evidence, deliver the warning, then persist intent before
    /// returning an action. A failed prerequisite never reaches the worker.
    async fn refresh(
        &mut self,
        saved: &mut Recovery,
        notifications: &NotificationSink,
        started: std::time::Instant,
    ) -> Result<Option<Policy>> {
        let policy = sleep::bounded(
            "read critical battery policy",
            Duration::from_secs(2),
            super::load(),
        )
        .await?
        .critical_battery;
        let sample = if policy.enabled {
            sample().await.map(Some)
        } else {
            Ok(None)
        };
        let sample_error = sample.as_ref().err().map(|e| format!("{e:#}"));
        let token = CANCELLATION.load(Ordering::SeqCst);
        let (warning, fire) = self.observe(
            &policy,
            sample.ok().flatten(),
            started.elapsed().as_secs(),
            token,
        );
        if warning {
            let result = sleep::bounded("critical battery warning", Duration::from_secs(2), async {
                notifications.send(IncomingNotification {
                    app_name: "Shelllist".into(), app_icon: "battery-caution".into(),
                    summary: "Critical battery: hibernation countdown".into(),
                    body: format!("At or below {}%. Hibernation will be requested in {} seconds unless power recovers. Cancel in Battery & Power. Screen locking and sleep inhibitors remain enforced.", policy.percent, policy.grace_seconds),
                    actions: vec![], hints: NotificationHints { urgency: 2, ..Default::default() },
                    expire_timeout: (policy.grace_seconds * 1000) as i32,
                }).await?; Ok(())
            }).await;
            self.warning_delivered(result, &policy, started.elapsed().as_secs());
        }
        if let Some(error) = sample_error {
            self.state.error = Some(error);
        }
        let recovery = Recovery {
            policy,
            attempted: self.latched,
        };
        if recovery != *saved {
            *saved = persist_recovery(recovery, token)
                .await
                .context("Cannot durably record critical battery intent")?;
        }
        Ok(fire.then_some(policy))
    }

    fn warning_delivered(&mut self, result: Result<()>, policy: &Policy, now: u64) {
        match result {
            Ok(()) => {
                // Start the full grace interval after delivery, not before a slow backend.
                self.deadline = Some(now.saturating_add(u64::from(policy.grace_seconds)));
                self.state.remaining_seconds = policy.grace_seconds;
            }
            Err(error) => self.stop(
                "blocked",
                Some(format!(
                    "Cannot deliver critical battery warning: {error:#}"
                )),
            ),
        }
        // A failed warning latches the episode before refresh persists recovery.
    }
}

#[cfg(test)]
mod tests {
    use super::{Engine, Policy, Recovery, Sample};
    fn low() -> Option<Sample> {
        Some(Sample {
            percent: 4.0,
            on_battery: true,
            discharging: true,
        })
    }
    fn policy() -> Policy {
        Policy {
            enabled: true,
            ..Default::default()
        }
    }
    #[test]
    fn warning_delivery_restarts_full_grace_and_failure_latches_before_persistence() {
        for delivered in [false, true] {
            let mut e = Engine::default();
            assert_eq!(e.observe(&Policy::default(), low(), 0, 0), (false, false));
            assert_eq!(e.observe(&policy(), low(), 0, 0), (true, false));
            e.warning_delivered(Ok(()), &policy(), 10);
            assert_eq!(e.observe(&policy(), low(), 69, 0), (false, false));
            if delivered {
                assert_eq!(e.observe(&policy(), low(), 70, 0), (false, true));
                e.stop("failed", Some("inhibited".into()));
            } else {
                e.warning_delivered(Err(anyhow::anyhow!("unavailable")), &policy(), 69);
                assert_eq!(e.state.phase, "blocked");
            }
            assert!(e.latched);
            assert_eq!(e.observe(&policy(), low(), 600, 0), (false, false));
            let saved = Recovery {
                policy: policy(),
                attempted: e.latched,
            };
            let restored: Recovery =
                serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
            let mut restarted = Engine {
                policy: Some(restored.policy),
                latched: restored.attempted,
                ..Default::default()
            };
            assert_eq!(
                restarted.observe(&policy(), low(), 10000, 0),
                (false, false)
            );
        }
    }

    #[test]
    fn cancellation_ac_recovery_and_unknown_evidence_never_fire_old_deadlines() {
        let p = policy();
        let mut e = Engine::default();
        e.observe(&p, low(), 0, 0);
        assert_eq!(e.observe(&p, low(), 60, 1), (false, false));
        assert_eq!(e.state.phase, "cancelled");
        let ac = Some(Sample {
            on_battery: false,
            ..low().unwrap()
        });
        e.observe(&p, ac, 70, 1);
        assert_eq!(e.observe(&p, low(), 80, 1), (true, false));
        e.observe(&p, None, 100, 1);
        assert_eq!(e.observe(&p, low(), 141, 1), (true, false));
        assert_eq!(e.state.remaining_seconds, 60);
    }
    #[test]
    fn boundary_validation_and_hysteresis_prevent_unsafe_settings_and_retry_loops() {
        for p in [
            Policy {
                percent: 0,
                ..policy()
            },
            Policy {
                percent: 21,
                ..policy()
            },
            Policy {
                grace_seconds: 0,
                ..policy()
            },
            Policy {
                grace_seconds: 301,
                ..policy()
            },
        ] {
            assert!(p.validate().is_err());
        }
        let mut e = Engine::default();
        e.observe(&policy(), low(), 0, 0);
        e.observe(&policy(), low(), 60, 0);
        e.observe(
            &policy(),
            Some(Sample {
                percent: 6.0,
                ..low().unwrap()
            }),
            70,
            0,
        );
        assert_eq!(e.observe(&policy(), low(), 80, 0), (false, false));
        e.observe(
            &policy(),
            Some(Sample {
                percent: 8.0,
                ..low().unwrap()
            }),
            90,
            0,
        );
        assert_eq!(e.observe(&policy(), low(), 100, 0), (true, false));
    }
}
