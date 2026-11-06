use super::{
    activity_wire::{self, *},
    read_wire,
};
use crate::{
    activity::{contract_examples, model::ActivityState},
    state::StateStore,
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
fn rejected<T: DeserializeOwned>(method: &str) -> Value {
    super::decode_request::<T>(Value::Null, method)
        .err()
        .unwrap()
}

pub(super) async fn fixture() -> Value {
    let range = contract_examples::range();
    let todo = range.todos[0].clone();
    let state = ActivityState {
        available: true,
        event_count: 1,
        incomplete_todo_count: 1,
        next_event: range.events.first().cloned(),
        ..Default::default()
    };
    let store = StateStore::default();
    store.update_activity(state.clone()).await;
    let snapshot = read_wire::snapshot(&store.snapshot().await);
    let mut cases = Map::new();
    let empty = contract_examples::empty_range();
    cases.insert("emptyRange".into(), json!({"method":"activity.queryRange", "params":RangeRequest { from_unix_ms:empty.from_unix_ms, to_unix_ms:empty.to_unix_ms }, "success":activity_wire::range(Ok(empty))}));
    cases.insert("range".into(), json!({"method":"activity.queryRange", "params":RangeRequest { from_unix_ms:range.from_unix_ms, to_unix_ms:range.to_unix_ms },
        "success":activity_wire::range(Ok(range)), "failed_params":{"from_unix_ms":0,"to_unix_ms":0},
        "failed":Failure::Query.reply(anyhow::anyhow!("to_unix_ms must be greater than from_unix_ms")), "rejected":rejected::<RangeRequest>("activity.queryRange")}));
    cases.insert("create".into(), json!({"method":"todos.create", "params":CreateTodoRequest {title:"  Plan review  ".into(), due_unix_ms:None, due_date:todo.due_date.clone(), priority:0},
        "success":activity_wire::todo(Ok(todo.clone()), Failure::Create), "failed_params":{"title":"   "},
        "failed":Failure::Create.reply(anyhow::anyhow!("todo title cannot be empty")), "rejected":rejected::<CreateTodoRequest>("todos.create")}));
    for (key, completed) in [("complete", true), ("reopen", false)] {
        let mut changed = todo.clone();
        changed.completed = completed;
        changed.completed_unix_ms = completed.then_some(todo.created_unix_ms + 1000);
        cases.insert(key.into(), json!({"method":"todos.complete", "params":CompleteTodoRequest {id:todo.id.clone(), completed},
            "success":activity_wire::todo(Ok(changed), Failure::Complete), "failed_params":{"id":"missing"},
            "failed":Failure::Complete.reply(anyhow::anyhow!("todo missing was not found")), "rejected":rejected::<CompleteTodoRequest>("todos.complete")}));
    }
    cases.insert("delete".into(), json!({"method":"todos.delete", "params":TodoRequest {id:todo.id.clone()},
        "success":activity_wire::deleted(todo.id, Ok(())), "failed_params":{"id":"missing"},
        "failed":Failure::Delete.reply(anyhow::anyhow!("todo missing was not found")), "rejected":rejected::<TodoRequest>("todos.delete")}));
    let mut refreshing = state.clone();
    refreshing.syncing = true;
    cases.insert("refresh".into(), json!({"method":"activity.refresh", "params":{}, "success":activity_wire::refreshed(refreshing.clone())}));
    let mut events = Map::new();
    for (key, state) in [("refreshing", refreshing), ("completed", state)] {
        let (_, mut receiver) = store.snapshot_and_subscribe().await;
        store.update_activity(state).await;
        events.insert(
            key.into(),
            read_wire::Event::changed("subscription-example", receiver.recv().await.unwrap())
                .into_value(),
        );
    }
    json!({"snapshot":snapshot, "cases":cases, "events":events})
}
