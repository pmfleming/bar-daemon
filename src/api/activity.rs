use crate::{
    activity::ActivityService,
    protocol::activity_wire::{
        self, CompleteTodoRequest, CreateTodoRequest, Failure, RangeRequest, TodoRequest,
    },
    state::StateStore,
};
use serde_json::Value;
use std::sync::Arc;

pub(super) struct ActivityApi {
    state: StateStore,
    activity: Arc<ActivityService>,
}
impl ActivityApi {
    pub(super) fn new(state: StateStore, activity: Arc<ActivityService>) -> Self {
        Self { state, activity }
    }
    pub(super) async fn dispatch(&self, method: &str, params: Value) -> Value {
        let result = match method {
            "activity.queryRange" => return self.activity_query_range(params).await,
            "activity.refresh" => return self.activity_refresh().await,
            "todos.create" => return self.todo_create(params).await,
            "todos.complete" => return self.todo_complete(params).await,
            "todos.delete" => return self.todo_delete(params).await,
            "locations.search" | "locations.add" | "locations.remove" => {
                self.locations(method, params).await
            }
            "location.status" | "location.setManual" | "location.returnToAutomatic" => {
                crate::timezone::location_control(method, params).await
            }
            _ => return super::unsupported_method(method),
        };
        match result {
            Ok(data) => super::success(data),
            Err(error) => super::error("location-error", error.to_string()),
        }
    }

    async fn locations(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Search {
            query: String,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Add {
            place: crate::location_place::Place,
            revision: u64,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Remove {
            id: String,
            revision: u64,
        }
        let id = match method {
            "locations.search" => {
                let request: Search = serde_json::from_value(params)?;
                return Ok(
                    serde_json::json!({"places":crate::activity::place_search::search(&request.query).await?}),
                );
            }
            "locations.add" => {
                let request: Add = serde_json::from_value(params)?;
                self.activity
                    .add_city(request.place, request.revision)
                    .await?
            }
            "locations.remove" => {
                let request: Remove = serde_json::from_value(params)?;
                self.activity
                    .remove_city(&request.id, request.revision)
                    .await?;
                request.id
            }
            _ => anyhow::bail!("Unknown location operation"),
        };
        Ok(serde_json::json!({"id":id,"activity":self.state.read(|s|s.activity.clone()).await}))
    }

    async fn activity_query_range(&self, params: Value) -> Value {
        let request = request!(params, RangeRequest, "activity.queryRange");
        activity_wire::range(
            self.activity
                .query_range(request.from_unix_ms, request.to_unix_ms)
                .await,
        )
    }
    async fn activity_refresh(&self) -> Value {
        self.activity.request_refresh().await;
        activity_wire::refreshed(self.state.read(|s| s.activity.clone()).await)
    }
    async fn todo_create(&self, params: Value) -> Value {
        let request = request!(params, CreateTodoRequest, "todos.create");
        activity_wire::todo(
            self.activity
                .create_todo(
                    request.title,
                    request.due_unix_ms,
                    request.due_date,
                    request.priority,
                )
                .await,
            Failure::Create,
        )
    }
    async fn todo_complete(&self, params: Value) -> Value {
        let request = request!(params, CompleteTodoRequest, "todos.complete");
        activity_wire::todo(
            self.activity
                .complete_todo(&request.id, request.completed)
                .await,
            Failure::Complete,
        )
    }
    async fn todo_delete(&self, params: Value) -> Value {
        let request = request!(params, TodoRequest, "todos.delete");
        let result = self.activity.delete_todo(&request.id).await;
        activity_wire::deleted(request.id, result)
    }
}

#[cfg(test)]
mod tests {
    use super::{ActivityApi, ActivityService, StateStore, Value};
    use serde_json::json;

    #[tokio::test]
    async fn location_api_requires_revision_and_never_removes_protected_roles() {
        let directory = tempfile::tempdir().unwrap();
        let state = StateStore::default();
        let service = ActivityService::with_paths(
            state.clone(),
            directory.path().join("config.json"),
            directory.path().join("todos.json"),
        )
        .await;
        let api = ActivityApi::new(state, service);
        let place = json!({
            "name": "Haarlem", "country": "Netherlands", "region": "North Holland",
            "timezone": "Europe/Amsterdam", "latitude": 52.38, "longitude": 4.64,
        });
        let reply = api
            .dispatch("locations.add", json!({"place":place,"revision":1}))
            .await;
        assert_eq!(reply["ok"], true);
        let id = reply["data"]["id"].as_str().unwrap();
        assert_eq!(reply["data"]["activity"]["location_catalog"]["revision"], 2);
        for protected in ["weather:home", "current-location"] {
            assert_eq!(
                api.dispatch("locations.remove", json!({"id":protected,"revision":2}))
                    .await["ok"],
                false
            );
        }
        assert_eq!(
            api.dispatch("locations.remove", json!({"id":id,"revision":1}))
                .await["ok"],
            false
        );
        assert_eq!(
            api.dispatch("locations.remove", json!({"id":id,"revision":2}))
                .await["ok"],
            true
        );
        assert_eq!(
            api.dispatch("locations.add", json!({"place":null,"revision":3}))
                .await["ok"],
            false
        );
    }

    #[tokio::test]
    async fn isolated_live_todo_lifecycle_matches_generated_replies_and_persists_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("todos.json");
        let state = StateStore::default();
        let service = ActivityService::with_paths(
            state.clone(),
            directory.path().join("config.json"),
            path.clone(),
        )
        .await;
        let api = ActivityApi::new(state.clone(), service);
        let fixture = crate::protocol::activity_contract_fixture().await;
        let cases = &fixture["cases"];
        let created = api.todo_create(cases["create"]["params"].clone()).await;
        let mut expected = cases["create"]["success"].clone();
        // IDs/timestamps are allocated by the live service, not fixture constants.
        let id = created["data"]["todo"]["id"].as_str().unwrap();
        let timestamp = &created["data"]["todo"]["created_unix_ms"];
        assert!(id.starts_with("local-"));
        assert!(timestamp.as_i64().unwrap() > 0);
        expected["data"]["todo"]["id"] = json!(id);
        expected["data"]["todo"]["created_unix_ms"] = timestamp.clone();
        assert_eq!(created, expected);
        for (key, completed) in [("complete", true), ("reopen", false)] {
            let reply = api
                .todo_complete(json!({"id":id, "completed":completed}))
                .await;
            let mut expected = cases[key]["success"].clone();
            expected["data"]["todo"]["id"] = json!(id);
            expected["data"]["todo"]["created_unix_ms"] = timestamp.clone();
            if completed {
                assert!(
                    reply["data"]["todo"]["completed_unix_ms"].as_i64().unwrap()
                        >= timestamp.as_i64().unwrap()
                );
                expected["data"]["todo"]["completed_unix_ms"] =
                    reply["data"]["todo"]["completed_unix_ms"].clone();
            }
            assert_eq!(reply, expected);
        }
        let range = api
            .activity_query_range(cases["range"]["params"].clone())
            .await;
        assert_eq!(
            range["data"]["activity_range"]["todos"],
            json!([expected["data"]["todo"].clone()])
        );
        assert_eq!(
            range["data"]["activity_range"]["days"]["2026-01-15"]["todo_ids"],
            json!([id])
        );
        assert_eq!(state.snapshot().await.activity.incomplete_todo_count, 1);
        let persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted.as_array().unwrap().len(), 1);
        assert_eq!(
            api.todo_delete(json!({"id":id})).await,
            crate::api::success(json!({"deleted":id}))
        );
        assert_eq!(state.snapshot().await.activity.incomplete_todo_count, 0);
        for (key, method) in [
            ("create", "todos.create"),
            ("complete", "todos.complete"),
            ("delete", "todos.delete"),
            ("range", "activity.queryRange"),
        ] {
            let case = &cases[key];
            for (params, expected) in [
                (case["failed_params"].clone(), &case["failed"]),
                (Value::Null, &case["rejected"]),
            ] {
                assert_eq!(api.dispatch(method, params).await, *expected);
            }
        }
        assert_eq!(
            api.dispatch("todos.unknown", json!({})).await,
            crate::api::unsupported_method("todos.unknown")
        );
        assert_eq!(
            api.dispatch("activity.refresh", json!({})).await["ok"],
            true
        );
        let persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted, json!([]));
    }
}
