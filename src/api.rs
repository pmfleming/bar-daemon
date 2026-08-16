use std::sync::Arc;

use serde_json::{Value, json};

use crate::{audio, brightness, hyprland::HyprlandClient, media, state::StateStore};

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
            "media.operation" => self.media_operation(params).await,
            "audio.adjust" => self.audio_adjust(params).await,
            "audio.setMuted" => self.audio_set_muted(params).await,
            "brightness.adjust" => self.brightness_adjust(params).await,
            "brightness.set" => self.brightness_set(params).await,
            _ => error(
                "unsupported-method",
                format!("Unsupported bar-api method: {method}"),
            ),
        }
    }

    async fn brightness_adjust(&self, params: Value) -> Value {
        let Some(delta) = params.get("delta_percent").and_then(Value::as_i64) else {
            return error(
                "validation-error",
                "brightness.adjust requires integer delta_percent",
            );
        };
        let Ok(delta) = i16::try_from(delta) else {
            return error(
                "validation-error",
                "brightness delta_percent is out of range",
            );
        };
        match brightness::adjust(delta).await {
            Ok(state) => success(json!({ "brightness": state })),
            Err(error_value) => error("brightness-operation-failed", error_value.to_string()),
        }
    }

    async fn brightness_set(&self, params: Value) -> Value {
        let Some(percent) = params.get("percent").and_then(Value::as_u64) else {
            return error(
                "validation-error",
                "brightness.set requires integer percent",
            );
        };
        let Ok(percent) = u8::try_from(percent) else {
            return error("validation-error", "brightness percent is out of range");
        };
        match brightness::set(percent).await {
            Ok(state) => success(json!({ "brightness": state })),
            Err(error_value) => error("brightness-operation-failed", error_value.to_string()),
        }
    }

    async fn audio_adjust(&self, params: Value) -> Value {
        let Some(delta) = params.get("delta_percent").and_then(Value::as_i64) else {
            return error(
                "validation-error",
                "audio.adjust requires integer delta_percent",
            );
        };
        let Ok(delta) = i16::try_from(delta) else {
            return error("validation-error", "audio delta_percent is out of range");
        };
        match audio::adjust(delta).await {
            Ok(state) => success(json!({ "audio": state })),
            Err(error_value) => error("audio-operation-failed", error_value.to_string()),
        }
    }

    async fn audio_set_muted(&self, params: Value) -> Value {
        let muted = match params.get("muted") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(value)) => Some(*value),
            _ => {
                return error(
                    "validation-error",
                    "audio.setMuted muted must be boolean or null",
                );
            }
        };
        match audio::set_muted(muted).await {
            Ok(state) => success(json!({ "audio": state })),
            Err(error_value) => error("audio-operation-failed", error_value.to_string()),
        }
    }

    async fn media_operation(&self, params: Value) -> Value {
        let Some(operation) = params.get("operation").and_then(Value::as_str) else {
            return error("validation-error", "media.operation requires operation");
        };
        let player_id = params.get("player_id").and_then(Value::as_str);
        match media::operation(player_id, operation).await {
            Ok(player_id) => success(json!({ "operation": operation, "player_id": player_id })),
            Err(error_value) => error("media-operation-failed", error_value.to_string()),
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
