use crate::{
    activity::ActivityService,
    protocol::activity_wire::{
        self, CompleteTodoRequest, CreateTodoRequest, Failure, RangeRequest, TodoRequest,
    },
    state::StateStore,
};
use serde_json::Value;
use std::sync::Arc;

// Once admitted, a mutation owns commit + cache publication independently of
// the transport waiter. Dropping a socket/D-Bus request must not cancel that
// publication after the database has committed.
async fn finish_mutation<T: Send + 'static>(
    operation: impl std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::spawn(operation).await.map_err(|error| {
        anyhow::anyhow!("agenda mutation owner failed; resolve outcome by receipt: {error}")
    })?
}

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
            "agenda.list" | "agenda.get" | "agenda.create" | "agenda.patch" | "agenda.delete"
            | "agenda.markers" | "agenda.receipt" | "agenda.schedule" => {
                return match self.agenda(method, params).await {
                    Ok(data) => super::success(data),
                    Err(error) => super::error("agenda-error", error.to_string()),
                };
            }
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

    async fn agenda(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        use crate::protocol::agenda_wire::*;
        let activity = Arc::clone(&self.activity);
        Ok(match method {
            "agenda.schedule" => {
                serde_json::json!({"schedule":crate::activity::agenda_schedule::parse(serde_json::from_value(params)?)?})
            }
            "agenda.list" => serde_json::to_value(
                self.activity
                    .agenda_list(serde_json::from_value(params)?)
                    .await?,
            )?,
            "agenda.markers" => serde_json::to_value(
                self.activity
                    .agenda_markers(serde_json::from_value(params)?)
                    .await?,
            )?,
            "agenda.get" => {
                let request: Get = serde_json::from_value(params)?;
                self.activity.agenda_get(&request.id).await?
            }
            "agenda.receipt" => {
                let request: Receipt = serde_json::from_value(params)?;
                self.activity.agenda_receipt(request.request_id).await?
            }
            "agenda.create" => {
                let r: Create = serde_json::from_value(params)?;
                serde_json::to_value(
                    finish_mutation(
                        async move { activity.agenda_create(r.request_id, r.item).await },
                    )
                    .await?,
                )?
            }
            "agenda.patch" => {
                let r: Patch = serde_json::from_value(params)?;
                serde_json::to_value(
                    finish_mutation(async move {
                        activity
                            .agenda_patch(r.request_id, r.id, r.expected_revision, r.patch)
                            .await
                    })
                    .await?,
                )?
            }
            "agenda.delete" => {
                let r: Delete = serde_json::from_value(params)?;
                serde_json::to_value(
                    finish_mutation(async move {
                        activity
                            .agenda_delete(r.request_id, r.id, r.expected_revision)
                            .await
                    })
                    .await?,
                )?
            }
            _ => anyhow::bail!("unsupported agenda method"),
        })
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
        let activity = Arc::clone(&self.activity);
        activity_wire::todo(
            finish_mutation(async move {
                activity
                    .create_todo(
                        request.title,
                        request.due_unix_ms,
                        request.due_date,
                        request.priority,
                    )
                    .await
            })
            .await,
            Failure::Create,
        )
    }
    async fn todo_complete(&self, params: Value) -> Value {
        let request = request!(params, CompleteTodoRequest, "todos.complete");
        let activity = Arc::clone(&self.activity);
        activity_wire::todo(
            finish_mutation(
                async move { activity.complete_todo(&request.id, request.completed).await },
            )
            .await,
            Failure::Complete,
        )
    }
    async fn todo_delete(&self, params: Value) -> Value {
        let request = request!(params, TodoRequest, "todos.delete");
        let activity = Arc::clone(&self.activity);
        let id = request.id.clone();
        let result = finish_mutation(async move { activity.delete_todo(&id).await }).await;
        activity_wire::deleted(request.id, result)
    }
}

#[cfg(test)]
#[path = "agenda_tests.rs"]
mod agenda_tests;

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
        assert!(id.starts_with("local:"));
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
        assert_eq!(range["data"]["activity_range"]["todos"], json!([]));
        let catalog = api.dispatch("agenda.list", json!({})).await;
        assert_eq!(catalog["data"]["items"][0]["id"], id);
        assert_eq!(state.snapshot().await.activity.incomplete_todo_count, 1);
        assert!(
            !path.exists(),
            "legacy JSON must not be created or dual-written"
        );
        let persisted = api.activity.data_snapshot_for_test().await;
        assert_eq!(persisted.items.len(), 1);
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
        assert!(!path.exists());
        assert!(api.activity.data_snapshot_for_test().await.items.is_empty());
    }
}
