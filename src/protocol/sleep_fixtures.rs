//! Offline fixtures: real tracker transitions, store commits and v1 serializers.
use super::{
    read_wire,
    sleep_wire::{self, Action, Failure, IdleRequest, KeepAwakeRequest},
};
use crate::{
    api,
    model::PowerSleepState,
    sleep,
    sleep_policy::{SleepPolicy, SleepPolicyState, critical},
    state::StateStore,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

fn rejection<T: DeserializeOwned>(method: &str) -> Value {
    api::decode_request::<T>(json!({"unexpected": true}), method)
        .err()
        .expect("unknown fields are rejected")
}
fn case(params: impl Serialize, success: Value, failed: Value, rejected: Option<Value>) -> Value {
    json!({"params": params, "success": success, "failed": failed, "rejected": rejected,
        "invalid_params": rejected.as_ref().map(|_| json!({"unexpected": true}))})
}
fn unavailable(failure: Failure) -> Value {
    failure.reply(&anyhow::anyhow!("service unavailable").context("Sleep control rejected"))
}
fn initial_state() -> PowerSleepState {
    PowerSleepState {
        available: true,
        can_suspend: "yes".into(),
        can_hibernate: "yes".into(),
        lock_before_sleep: true,
        ..Default::default()
    }
}

pub(super) async fn fixture() -> Value {
    let initial = StateStore::default();
    initial.update_power_sleep(initial_state()).await;
    let mut policy = SleepPolicyState {
        available: true,
        active_profile: "shared".into(),
        hibernate_available: true,
        hibernate_ready: true,
        ..Default::default()
    };
    policy.policy.critical_battery.enabled = true;
    policy.critical_battery.phase = "armed".into();
    initial.update_sleep_policy(policy.clone()).await;
    let snapshot = read_wire::snapshot(&initial.snapshot().await);
    let mut replies = Map::new();
    let mut events = Map::new();
    let mut recovery = Map::new();
    for action in [Action::Lock, Action::Suspend, Action::Hibernate] {
        let store = StateStore::default();
        store.update_power_sleep(initial_state()).await;
        let mut changed = store.subscribe();
        let mut lifecycle = Map::new();
        let mut acknowledged = None;
        if !matches!(action, Action::Lock) {
            for (name, operation) in sleep::contract_operations(action.as_str()) {
                store.record_sleep_preparation(name == "preparing").await;
                if name == "returned" {
                    store.record_resume().await;
                }
                store.record_sleep_operation(operation).await;
                let mut latest = None;
                while let Ok(event) = changed.try_recv() {
                    latest = Some(event);
                }
                lifecycle.insert(
                    name.clone(),
                    read_wire::Event::changed(
                        "subscription-example",
                        latest.expect("operation commit emits an event"),
                    )
                    .into_value(),
                );
                if name == "accepted" {
                    acknowledged =
                        Some(sleep_wire::publish(&store, initial_state(), Some(action)).await);
                }
            }
            recovery.insert(
                action.as_str().into(),
                read_wire::snapshot(&store.snapshot().await),
            );
        }
        let success = match acknowledged {
            Some(reply) => reply,
            None => sleep_wire::publish(&store, initial_state(), Some(action)).await,
        };
        let mut example = case(json!({}), success, unavailable(Failure::Operation), None);
        if !lifecycle.is_empty() {
            let message = store
                .read(|s| s.power_sleep.operation.error.clone())
                .await
                .unwrap();
            example["uncertain"] = Failure::Operation.reply(&anyhow::anyhow!(message));
            events.insert(action.as_str().into(), Value::Object(lifecycle));
        }
        replies.insert(format!("powerSleep.{}", action.as_str()), example);
    }
    let store = StateStore::default();
    let mut awake = initial_state();
    awake.keep_awake = true;
    replies.insert(
        "powerSleep.setKeepAwake".into(),
        case(
            KeepAwakeRequest { enabled: true },
            sleep_wire::publish(&store, awake, None).await,
            unavailable(Failure::KeepAwake),
            Some(rejection::<KeepAwakeRequest>("powerSleep.setKeepAwake")),
        ),
    );
    let accepted = sleep::contract_operations("suspend")
        .into_iter()
        .find(|(name, _)| name == "accepted")
        .unwrap()
        .1;
    store.record_sleep_operation(accepted).await;
    replies.insert(
        "powerSleep.idle".into(),
        case(
            IdleRequest {
                sleep_minutes: 30,
                generation: "generation-example".into(),
                episode: 1,
            },
            sleep_wire::publish(&store, initial_state(), None).await,
            unavailable(Failure::Automatic),
            Some(rejection::<IdleRequest>("powerSleep.idle")),
        ),
    );

    let mut updated = policy.clone();
    updated.policy.battery.sleep_minutes = 15;
    let mut requested = updated.policy.clone();
    requested.critical_battery.enabled = false; // General policy writes retain stored critical settings.
    replies.insert(
        "powerSleep.setPolicy".into(),
        case(
            requested,
            sleep_wire::policy_result(Ok(updated), Failure::Policy),
            unavailable(Failure::Policy),
            Some(rejection::<SleepPolicy>("powerSleep.setPolicy")),
        ),
    );
    let mut updated = policy.clone();
    updated.policy.critical_battery.percent = 10;
    replies.insert(
        "powerSleep.setCriticalPolicy".into(),
        case(
            updated.policy.critical_battery,
            sleep_wire::policy_result(Ok(updated), Failure::CriticalPolicy),
            unavailable(Failure::CriticalPolicy),
            Some(rejection::<critical::Policy>(
                "powerSleep.setCriticalPolicy",
            )),
        ),
    );

    initial
        .update_critical_battery(critical::State {
            phase: "countdown".into(),
            remaining_seconds: 45,
            error: None,
        })
        .await;
    let mut changes = initial.subscribe();
    initial
        .update_critical_battery(critical::cancellation_state("countdown").unwrap())
        .await;
    let cancelled_event =
        read_wire::Event::changed("subscription-example", changes.recv().await.unwrap())
            .into_value();
    let cancelled = initial.read(|s| sleep_wire::policy(&s.sleep_policy)).await;
    let no_countdown = critical::cancellation_state("armed").unwrap_err();
    replies.insert(
        "powerSleep.cancelCritical".into(),
        case(
            json!({}),
            cancelled,
            Failure::CriticalCancel.reply(&no_countdown),
            None,
        ),
    );
    // The countdown seed is typed; the wire projection still comes from the store.
    initial
        .update_critical_battery(critical::State {
            phase: "countdown".into(),
            remaining_seconds: 45,
            error: None,
        })
        .await;
    let countdown_event =
        read_wire::Event::changed("subscription-example", changes.recv().await.unwrap())
            .into_value();
    json!({"snapshot": snapshot, "replies": replies, "events": events, "recovery": recovery,
        "critical": {"countdown": countdown_event, "cancelled": cancelled_event}})
}
