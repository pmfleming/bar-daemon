//! Offline examples pass typed observations through live reply, store and event serializers.
//! No compositor, persistence or transaction executor is invoked here.
use super::{
    display_wire::{self, Empty, Failure, Patch, Token},
    read_wire,
};
use crate::{
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
    let store = StateStore::default();
    let mut captured = std::collections::BTreeMap::new();
    let workflow = [
        "initial",
        "preview",
        "confirmed",
        "reverted",
        "focus",
        "focusReset",
        "policy",
    ];
    let names: Vec<_> = workflow
        .iter()
        .copied()
        .chain(
            states
                .keys()
                .map(String::as_str)
                .filter(|name| !workflow.contains(name)),
        )
        .collect();
    for name in names {
        let state = &states[name];
        let mut changes = store.subscribe();
        store.update_display_policy(state.clone()).await;
        captured.insert(name.to_owned(), store.snapshot().await);
        events.insert(
            name.to_owned(),
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
        .contains(&name)
        {
            snapshots.insert(name.to_owned(), read_wire::snapshot(&captured[name]));
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
            "displayLayout.preview" => {
                let normalized = serde_json::to_value(Preview {
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
                .unwrap();
                let mut draft = normalized.clone();
                let observed = serde_json::to_value(&states["initial"]).unwrap();
                for setting in draft["outputs"].as_array_mut().unwrap() {
                    let output = observed["outputs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|o| o["name"] == setting["name"])
                        .unwrap();
                    for key in ["modes", "internal", "width", "height"] {
                        setting[key] = output[key].clone();
                    }
                }
                assert_eq!(
                    serde_json::to_value(serde_json::from_value::<Preview>(draft.clone()).unwrap())
                        .unwrap(),
                    normalized
                );
                (draft, rejected::<Preview>(failure))
            }
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
                super::decode_request::<DisplayPolicy>(json!({"unknown": true}), method)
                    .unwrap_err(),
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
            "success": read_wire::reconcile_reply(display_wire::reply(Ok(states[state].clone()), failure), &captured[state]),
            "failed": display_wire::reply(Err(anyhow::anyhow!("compositor unavailable").context("Display control failed")), failure),
            "uncertain": failure.reply(&anyhow::anyhow!("deadline has elapsed").context(uncertain)),
            "invalid_params": {"unknown": true}, "rejected": rejected, "recovery": state,
        }));
    }
    let mut invalid_draft = replies["displayLayout.preview"]["params"].clone();
    invalid_draft["outputs"][0]["scale"] = json!("");
    let error = serde_json::from_value::<Preview>(invalid_draft.clone())
        .err()
        .unwrap();
    json!({"snapshots": snapshots, "events": events, "replies": replies,
        "invalidDraft": {"params":invalid_draft, "reply":Failure::Layout.reply(&error.into())}})
}
