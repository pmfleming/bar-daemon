//! Offline examples pass typed observations through live reply, store and event serializers.
//! No compositor, persistence or transaction executor is invoked here.
use super::{
    display_wire::{self, Empty, Failure, Patch, Token},
    read_wire,
};
use crate::{
    api,
    display_policy::{DisplayPolicy, contract_examples, layout::Preview},
    state::StateStore,
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

fn rejected<T: DeserializeOwned>(failure: Failure) -> Value {
    let error = serde_json::from_value::<T>(json!({"unknown": true}))
        .err()
        .expect("strict request schema");
    failure.reply(&error.into())
}

pub(super) async fn fixture() -> Value {
    let states = contract_examples::states();
    let mut events = Map::new();
    let mut snapshots = Map::new();
    for (name, state) in &states {
        let store = StateStore::default();
        let mut changes = store.subscribe();
        store.update_display_policy(state.clone()).await;
        events.insert(
            name.clone(),
            read_wire::Event::changed(
                "subscription-example",
                changes.try_recv().expect("display commit emits an event"),
            )
            .into_value(),
        );
        if [
            "initial",
            "preview",
            "confirmed",
            "reverted",
            "focus",
            "focusReset",
            "policy",
        ]
        .contains(&name.as_str())
        {
            snapshots.insert(name.clone(), read_wire::snapshot(&store.snapshot().await));
        }
    }
    let mut replies = Map::new();
    for (method, state, failure) in [
        ("displayLayout.preview", "preview", Failure::Layout),
        ("displayLayout.confirm", "confirmed", Failure::Layout),
        ("displayLayout.revert", "reverted", Failure::Layout),
        ("displayFocus.set", "focus", Failure::Focus),
        ("displayFocus.reset", "focusReset", Failure::Focus),
        ("displayPolicy.set", "policy", Failure::Policy),
    ] {
        let (params, rejected) = match method {
            "displayLayout.preview" => (
                serde_json::to_value(Preview {
                    baseline: states["initial"].baseline.clone(),
                    outputs: states["preview"]
                        .layout
                        .trial
                        .as_ref()
                        .unwrap()
                        .proposed
                        .outputs
                        .clone(),
                })
                .unwrap(),
                rejected::<Preview>(failure),
            ),
            "displayLayout.confirm" | "displayLayout.revert" => (
                serde_json::to_value(Token {
                    id: "trial-example".into(),
                })
                .unwrap(),
                rejected::<Token>(failure),
            ),
            "displayFocus.set" => (
                serde_json::to_value(Patch {
                    values: states["focus"].focus.saved.clone(),
                })
                .unwrap(),
                rejected::<Patch>(failure),
            ),
            "displayFocus.reset" => (
                serde_json::to_value(Empty {}).unwrap(),
                rejected::<Empty>(failure),
            ),
            "displayPolicy.set" => (
                serde_json::to_value(&states["policy"].policy).unwrap(),
                api::decode_request::<DisplayPolicy>(json!({"unknown": true}), method).unwrap_err(),
            ),
            _ => unreachable!(),
        };
        let uncertain = match failure {
            Failure::Layout => "Display layout request timed out; unconfirmed changes will revert",
            Failure::Focus => "Focus settings request timed out",
            Failure::Policy => "Preference persistence outcome unknown",
        };
        replies.insert(method.into(), json!({
            "params": params,
            "success": display_wire::reply(Ok(states[state].clone()), failure),
            "failed": display_wire::reply(Err(anyhow::anyhow!("compositor unavailable").context("Display control failed")), failure),
            "uncertain": failure.reply(&anyhow::anyhow!("deadline has elapsed").context(uncertain)),
            "invalid_params": {"unknown": true}, "rejected": rejected, "recovery": state,
        }));
    }
    json!({"snapshots": snapshots, "events": events, "replies": replies})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replies_events_and_recovery_snapshots_share_the_normalized_contract() {
        let fixture = fixture().await;
        assert_eq!(fixture, super::fixture().await);
        assert_eq!(fixture["replies"].as_object().unwrap().len(), 6);
        for (method, case) in fixture["replies"].as_object().unwrap() {
            assert!(super::super::METHODS.contains(&method.as_str()));
            let state = case["recovery"].as_str().unwrap();
            let data = &case["success"]["data"]["display_policy"];
            assert_eq!(data, &fixture["events"][state]["data"]);
            assert_eq!(
                data,
                &fixture["snapshots"][state]["data"]["snapshot"]["display_policy"]
            );
            assert_eq!(fixture["events"][state]["stream"], "display-policy.changed");
            assert_eq!(
                case["rejected"]["error"]["code"],
                if method == "displayPolicy.set" {
                    "validation-error"
                } else if method.starts_with("displayLayout.") {
                    "display-layout-failed"
                } else {
                    "display-focus-failed"
                }
            );
        }
        assert!(fixture["events"]["preview"]["data"]["layout"]["trial"].is_object());
        assert!(fixture["events"]["confirmed"]["data"]["layout"]["trial"].is_null());
        assert_eq!(
            fixture["events"]["rollback-failed"]["data"]["docking_state"],
            "failed"
        );
    }
}
