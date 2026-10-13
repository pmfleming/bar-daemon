//! Additive v1 operation API. Legacy immediate methods retain their reply shape.
//! IDs/status belong to the initiating bus owner, not a frontend route string.
use crate::{
    api::{self, ApiService},
    battery::authorization,
};
use futures::FutureExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;

const RETENTION: Duration = Duration::from_secs(300);
const OPERATION_BUDGET: Duration = Duration::from_secs(300);
const CAPACITY: usize = 64;
const METHODS: &[&str] = &[
    "battery.setThresholds",
    "battery.setProtection",
    "battery.chargeOnce",
    "battery.setChargingInhibited",
    "battery.startCalibration",
    "battery.cancelCalibration",
];

#[derive(Default, Clone)]
pub(super) struct Operations(Arc<Mutex<Store>>);
#[derive(Default)]
struct Store {
    next: u64,
    entries: HashMap<String, Entry>,
}
struct Entry {
    owner: String,
    cancel: watch::Sender<bool>,
    response: Option<Value>,
    finished: Option<Instant>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    method: String,
    params: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    operation_id: String,
}

impl Operations {
    pub(super) fn call(
        &self,
        method: &str,
        params: Value,
        owner: &str,
        connection: &zbus::Connection,
        api: Arc<ApiService>,
    ) -> Value {
        if method == "battery.operation.start" {
            let request: Start = match serde_json::from_value(params) {
                Ok(value) => value,
                Err(error) => return api::error("validation-error", error.to_string()),
            };
            if !METHODS.contains(&request.method.as_str()) {
                return api::error(
                    "validation-error",
                    "method is not an interactive battery operation",
                );
            }
            return self.start(owner, connection, async move {
                api.dispatch(&request.method, request.params).await
            });
        }
        self.lookup(method, params, owner)
    }

    fn start(
        &self,
        owner: &str,
        connection: &zbus::Connection,
        work: impl std::future::Future<Output = Value> + Send + 'static,
    ) -> Value {
        let mut store = self.0.lock().unwrap();
        store
            .entries
            .retain(|_, e| e.finished.is_none_or(|time| time.elapsed() < RETENTION));
        if store.entries.len() >= CAPACITY || store.entries.values().any(|e| e.response.is_none()) {
            return api::error(
                "operation-active",
                "battery operation capacity exceeded; request was not dispatched",
            );
        }
        store.next += 1;
        // Include bus and service generations: a restart must never make
        // an old opaque ID refer to a new operation by the same caller.
        let id = format!(
            "battery-operation-{}-{}-{}",
            connection.server_guid(),
            connection
                .unique_name()
                .map(|name| name.as_str())
                .unwrap_or("peer"),
            store.next
        );
        let (cancel, receiver) = watch::channel(false);
        store.entries.insert(
            id.clone(),
            Entry {
                owner: owner.into(),
                cancel: cancel.clone(),
                response: None,
                finished: None,
            },
        );
        let operations = self.clone();
        let task_id = id.clone();
        let owner = owner.to_string();
        let connection = connection.clone();
        tokio::spawn(async move {
            let monitor = shelllist_daemon_tokio::OwnerLossMonitor::new(connection);
            if !matches!(monitor.has_owner(&owner).await, Ok(true)) {
                operations.0.lock().unwrap().entries.remove(&task_id);
                return;
            }
            let work = async {
                std::panic::AssertUnwindSafe(authorization::INTERACTIVE.scope(receiver, work))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        api::error(
                            "outcome-unknown",
                            "battery operation task failed; reconcile state",
                        )
                    })
            };
            tokio::pin!(work);
            let (response, owner_lost) = tokio::select! {
                response = &mut work => (response, false),
                _ = monitor.wait(&owner) => {
                    cancel.send_replace(true);
                    (work.await, true)
                }
                _ = tokio::time::sleep(OPERATION_BUDGET) => {
                    cancel.send_replace(true);
                    (work.await, false)
                }
            };
            let mut store = operations.0.lock().unwrap();
            if owner_lost {
                store.entries.remove(&task_id);
            } else if let Some(entry) = store.entries.get_mut(&task_id) {
                entry.response = Some(response);
                entry.finished = Some(Instant::now());
            }
        });
        started(&id)
    }

    fn lookup(&self, method: &str, params: Value, owner: &str) -> Value {
        let request: Lookup = match serde_json::from_value(params) {
            Ok(value) => value,
            Err(error) => return api::error("validation-error", error.to_string()),
        };
        let mut store = self.0.lock().unwrap();
        let Some(entry) = store.entries.get_mut(&request.operation_id).filter(|e| {
            e.owner == owner && e.finished.is_none_or(|time| time.elapsed() < RETENTION)
        }) else {
            return api::error(
                "operation-not-found",
                "operation expired or belongs to another owner; reconcile state",
            );
        };
        if method == "battery.operation.cancel" {
            // Acknowledges intent, never claims the effect was undone. Keep
            // running until the API has reconciled/retained its durable state.
            entry.cancel.send_replace(true);
        }
        status(
            &request.operation_id,
            entry.response.clone(),
            *entry.cancel.borrow(),
        )
    }
}

pub(crate) fn started(id: &str) -> Value {
    status(id, None, false)
}
pub(crate) fn status(id: &str, response: Option<Value>, cancellation_requested: bool) -> Value {
    api::success(json!({ "operation": {
        "id": id, "state": if response.is_some() { "finished" } else { "running" },
        "cancellation_requested": cancellation_requested, "response": response,
        "retention_ms": RETENTION.as_millis() as u64,
    }}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_ack_admission_and_owner_loss_cancel_without_replay() {
        let bus = crate::battery::helper::authorization_tests::Bus::new();
        let server = bus.connection().await;
        let caller = bus.connection().await;
        let owner = caller.unique_name().unwrap().to_string();
        let operations = Operations::default();
        let (entered, started) = tokio::sync::oneshot::channel();
        let accepted = operations.start(&owner, &server, async move {
            entered.send(()).unwrap();
            let mut cancellation = authorization::cancellation().unwrap();
            authorization::cancelled(&mut cancellation).await;
            api::error(
                "outcome-unknown",
                "already-started effect is not rolled back",
            )
        });
        assert_eq!(accepted["data"]["operation"]["state"], "running");
        let id = accepted["data"]["operation"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        tokio::time::timeout(Duration::from_secs(1), started)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            operations.start(&owner, &server, async {
                panic!("rejected work must not execute")
            })["error"]["code"],
            "operation-active"
        );
        let response = operations.lookup(
            "battery.operation.cancel",
            json!({"operation_id": id}),
            &owner,
        );
        assert_eq!(response["data"]["operation"]["state"], "running");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let response = operations.lookup(
                    "battery.operation.status",
                    json!({"operation_id": id}),
                    &owner,
                );
                if response["data"]["operation"]["state"] == "finished" {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let accepted = operations.start(&owner, &server, async {
            let mut cancellation = authorization::cancellation().unwrap();
            authorization::cancelled(&mut cancellation).await;
            api::error("outcome-unknown", "owner lost")
        });
        let id = accepted["data"]["operation"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        caller.close().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while operations.0.lock().unwrap().entries.contains_key(&id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn status_cancellation_retention_and_owner_scope() {
        let operations = Operations::default();
        let (cancel, receiver) = watch::channel(false);
        operations.0.lock().unwrap().entries.insert(
            "operation".into(),
            Entry {
                owner: ":1.1".into(),
                cancel,
                response: None,
                finished: None,
            },
        );
        let params = json!({"operation_id": "operation"});
        for method in ["battery.operation.status", "battery.operation.cancel"] {
            let denied = operations.lookup(method, params.clone(), ":1.2");
            assert_eq!(denied["error"]["code"], "operation-not-found");
        }
        assert!(!*receiver.borrow());
        let running = operations.lookup("battery.operation.status", params.clone(), ":1.1");
        assert_eq!(running, started("operation"));
        let cancellation = operations.lookup("battery.operation.cancel", params.clone(), ":1.1");
        assert_eq!(cancellation["data"]["operation"]["state"], "running");
        assert_eq!(
            cancellation["data"]["operation"]["cancellation_requested"],
            true
        );
        assert!(*receiver.borrow());
        let response = api::error(
            "outcome-unknown",
            "already-dispatched write may have completed",
        );
        {
            let mut store = operations.0.lock().unwrap();
            let entry = store.entries.get_mut("operation").unwrap();
            entry.response = Some(response.clone());
            entry.finished = Some(Instant::now());
        }
        let terminal = operations.lookup("battery.operation.status", params.clone(), ":1.1");
        assert_eq!(terminal["data"]["operation"]["response"], response);
        operations
            .0
            .lock()
            .unwrap()
            .entries
            .get_mut("operation")
            .unwrap()
            .finished = Some(Instant::now() - RETENTION);
        assert_eq!(
            operations.lookup("battery.operation.status", params, ":1.1")["error"]["code"],
            "operation-not-found"
        );
    }
}
