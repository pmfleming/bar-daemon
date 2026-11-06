//! v1 sleep replies. Publishing must retain the store-owned operation and resume generation.
use crate::{model::PowerSleepState, sleep_policy::SleepPolicyState, state::StateStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Action {
    Lock,
    Suspend,
    Hibernate,
}
impl Action {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Suspend => "suspend",
            Self::Hibernate => "hibernate",
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KeepAwakeRequest {
    pub enabled: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdleRequest {
    pub sleep_minutes: u32,
    pub generation: String,
    pub episode: u64,
}

#[derive(Serialize)]
struct SleepReply<'a> {
    power_sleep: &'a PowerSleepState,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<Action>,
}
#[derive(Serialize)]
struct PolicyReply<'a> {
    sleep_policy: &'a SleepPolicyState,
}

pub(crate) enum Failure {
    Operation,
    KeepAwake,
    Policy,
    CriticalPolicy,
    CriticalCancel,
    Automatic,
}
impl Failure {
    pub(crate) fn reply(self, error: &anyhow::Error) -> Value {
        let code = match self {
            Self::Operation => "power-sleep-operation-failed",
            Self::KeepAwake => "keep-awake-failed",
            Self::Policy => "sleep-policy-failed",
            Self::CriticalPolicy => "critical-battery-policy-failed",
            Self::CriticalCancel => "critical-battery-cancel-failed",
            Self::Automatic => "automatic-sleep-failed",
        };
        super::error(code, format!("{error:#}"))
    }
}

pub(crate) async fn publish(
    store: &StateStore,
    state: PowerSleepState,
    action: Option<Action>,
) -> Value {
    store.update_power_sleep(state).await;
    store
        .read(|s| {
            super::serialized_success(SleepReply {
                power_sleep: &s.power_sleep,
                operation: action,
            })
        })
        .await
}

pub(crate) fn policy(state: &SleepPolicyState) -> Value {
    super::serialized_success(PolicyReply {
        sleep_policy: state,
    })
}
pub(crate) fn policy_result(result: anyhow::Result<SleepPolicyState>, failure: Failure) -> Value {
    match result {
        Ok(state) => policy(&state),
        Err(error) => failure.reply(&error),
    }
}
