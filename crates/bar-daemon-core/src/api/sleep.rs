//! Sleep API routing and publication stay together, independent of desktop controls.
use crate::{
    protocol::sleep_wire::{self, Action, Failure, IdleRequest, KeepAwakeRequest},
    sleep, sleep_policy,
    state::StateStore,
};
use serde_json::Value;

pub(super) async fn dispatch(state: &StateStore, method: &str, params: Value) -> Value {
    match method {
        "powerSleep.lock" => perform(state, Action::Lock).await,
        "powerSleep.suspend" => perform(state, Action::Suspend).await,
        "powerSleep.hibernate" => perform(state, Action::Hibernate).await,
        "powerSleep.setKeepAwake" => {
            let request = request!(params, KeepAwakeRequest, "powerSleep.setKeepAwake");
            publish(
                state,
                sleep::set_keep_awake(request.enabled).await,
                Failure::KeepAwake,
                None,
            )
            .await
        }
        "powerSleep.setPolicy" => {
            let policy = request!(params, sleep_policy::SleepPolicy, "powerSleep.setPolicy");
            sleep_wire::policy_result(sleep_policy::set(policy, state).await, Failure::Policy)
        }
        "powerSleep.setCriticalPolicy" => {
            let policy = request!(
                params,
                sleep_policy::critical::Policy,
                "powerSleep.setCriticalPolicy"
            );
            sleep_wire::policy_result(
                sleep_policy::critical::set_policy(policy, state).await,
                Failure::CriticalPolicy,
            )
        }
        "powerSleep.cancelCritical" => match sleep_policy::critical::cancel(state).await {
            Ok(_) => state.read(|s| sleep_wire::policy(&s.sleep_policy)).await,
            Err(error) => Failure::CriticalCancel.reply(&error),
        },
        "powerSleep.idle" => idle(state, params).await,
        _ => super::unsupported_method(method),
    }
}

async fn perform(state: &StateStore, action: Action) -> Value {
    publish(
        state,
        sleep::perform(action.as_str()).await,
        Failure::Operation,
        Some(action),
    )
    .await
}

async fn idle(state: &StateStore, params: Value) -> Value {
    let request = request!(params, IdleRequest, "powerSleep.idle");
    let result = sleep_policy::idle_sleep(
        request.sleep_minutes,
        &request.generation,
        request.episode,
        state,
    )
    .await;
    publish(state, result, Failure::Automatic, None).await
}

async fn publish(
    store: &StateStore,
    result: anyhow::Result<crate::model::PowerSleepState>,
    failure: Failure,
    action: Option<Action>,
) -> Value {
    match result {
        Ok(state) => sleep_wire::publish(store, state, action).await,
        Err(error) => failure.reply(&error),
    }
}
