use std::sync::Arc;

use serde_json::{Value, json};

use crate::{hyprland::HyprlandClient, state::StateStore};

pub const PROTOCOL: &str = "bar-api";
pub const VERSION: u8 = 1;
pub const BUS_NAME: &str = "org.laufan.BarDaemon";
pub const OBJECT_PATH: &str = "/org/laufan/BarDaemon";
pub const INTERFACE: &str = "org.laufan.BarDaemon1";

pub fn success(data: Value) -> Value {
    json!({ "protocol": PROTOCOL, "version": VERSION, "ok": true, "data": data })
}

pub fn error(code: &str, message: impl Into<String>) -> Value {
    json!({
        "protocol": PROTOCOL,
        "version": VERSION,
        "ok": false,
        "error": { "code": code, "message": message.into() }
    })
}

#[derive(Clone)]
pub struct ApiService {
    state: StateStore,
    hyprland: Arc<HyprlandClient>,
}

impl ApiService {
    pub fn new(state: StateStore) -> Self {
        Self {
            state,
            hyprland: Arc::new(HyprlandClient::default()),
        }
    }

    pub async fn dispatch(&self, method: &str, params: Value) -> Value {
        match method {
            "bar.snapshot" => success(json!({ "snapshot": self.state.snapshot().await })),
            "workspace.focus" => self.focus_workspace(params).await,
            _ => error(
                "unsupported-method",
                format!("Unsupported bar-api method: {method}"),
            ),
        }
    }

    async fn focus_workspace(&self, params: Value) -> Value {
        let Some(workspace_id) = params.get("workspace_id").and_then(Value::as_i64) else {
            return error(
                "validation-error",
                "workspace.focus requires integer workspace_id",
            );
        };
        let on_current_monitor = params
            .get("on_current_monitor")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match self
            .hyprland
            .focus_workspace(workspace_id, on_current_monitor)
            .await
        {
            Ok(()) => success(json!({
                "operation": "workspace.focus",
                "workspace_id": workspace_id,
                "on_current_monitor": on_current_monitor
            })),
            Err(error_value) => error("workspace-focus-failed", error_value.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ApiService;
    use crate::state::StateStore;

    #[tokio::test]
    async fn rejects_invalid_workspace_focus() {
        let api = ApiService::new(StateStore::default());
        let response = api.dispatch("workspace.focus", json!({})).await;
        assert_eq!(response["error"]["code"], "validation-error");
    }

    #[tokio::test]
    async fn returns_versioned_snapshot() {
        let api = ApiService::new(StateStore::default());
        let response = api.dispatch("bar.snapshot", json!({})).await;
        assert_eq!(response["protocol"], "bar-api");
        assert_eq!(response["version"], 1);
        assert_eq!(response["ok"], true);
    }
}
