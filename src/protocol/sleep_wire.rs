//! v1 sleep replies. Publishing must retain the store-owned operation and resume generation.
use crate::{api, model::PowerSleepState, sleep_policy::SleepPolicyState, state::StateStore};
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
        api::error(code, format!("{error:#}"))
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
            api::success(
                serde_json::to_value(SleepReply {
                    power_sleep: &s.power_sleep,
                    operation: action,
                })
                .expect("serializable sleep reply"),
            )
        })
        .await
}

pub(crate) fn policy(state: &SleepPolicyState) -> Value {
    api::success(
        serde_json::to_value(PolicyReply {
            sleep_policy: state,
        })
        .expect("serializable sleep policy reply"),
    )
}
pub(crate) fn policy_result(result: anyhow::Result<SleepPolicyState>, failure: Failure) -> Value {
    match result {
        Ok(state) => policy(&state),
        Err(error) => failure.reply(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SleepOperation;
    use serde_json::json;

    #[tokio::test]
    async fn publishing_stale_telemetry_keeps_outcome_and_resume_generation() {
        let store = StateStore::default();
        let operation = SleepOperation {
            id: 7,
            action: "hibernate".into(),
            phase: "unknown".into(),
            ..Default::default()
        };
        store.record_sleep_operation(operation.clone()).await;
        store.record_resume().await;
        for action in [
            Some(Action::Lock),
            Some(Action::Suspend),
            Some(Action::Hibernate),
            None,
        ] {
            let reply = publish(&store, PowerSleepState::default(), action).await;
            let state = store.snapshot().await.power_sleep;
            assert_eq!(state.operation, operation);
            assert_eq!(state.resume_generation, 1);
            let expected = match action {
                Some(action) => {
                    api::success(json!({"power_sleep": state, "operation": action.as_str()}))
                }
                None => api::success(json!({"power_sleep": state})),
            };
            assert_eq!(reply, expected);
        }
    }
}
