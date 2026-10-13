//! Media, workspace and update-refresh v1 contracts. Effects remain in their domains.
use crate::{
    api,
    model::{MediaControlMode, MediaState, UpdateState},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Serialize)]
pub(crate) struct MediaRequest {
    pub operation: String,
    #[serde(default)]
    pub player_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<MediaControlMode>,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct WorkspaceRequest {
    pub workspace_id: i64,
    #[serde(default)]
    pub on_current_monitor: bool,
}
#[derive(Serialize)]
#[serde(untagged)]
enum Reply<'a> {
    Media {
        operation: &'a str,
        player_id: Option<&'a str>,
        media: &'a MediaState,
    },
    Playback {
        operation: &'a str,
        player_id: &'a str,
    },
    Seek {
        operation: &'a str,
        player_id: &'a str,
        offset_seconds: i64,
    },
    Workspace {
        operation: &'static str,
        workspace_id: i64,
        on_current_monitor: bool,
    },
    Updates {
        updates: &'a UpdateState,
    },
}
impl Reply<'_> {
    fn envelope(self) -> Value {
        api::success(serde_json::to_value(self).expect("serializable session control reply"))
    }
}
#[derive(Clone, Copy)]
pub(crate) enum ErrorCode {
    Media,
    Workspace,
    Updates,
    Validation,
}
impl ErrorCode {
    pub(crate) fn reply(self, message: impl Into<String>) -> Value {
        api::error(
            match self {
                Self::Media => "media-operation-failed",
                Self::Workspace => "workspace-focus-failed",
                Self::Updates => "update-refresh-failed",
                Self::Validation => "validation-error",
            },
            message,
        )
    }
}
pub(crate) fn preference(operation: &str, result: anyhow::Result<MediaState>) -> Value {
    match result {
        Ok(state) => Reply::Media {
            operation,
            player_id: state.active_player.as_deref(),
            media: &state,
        }
        .envelope(),
        Err(error) => ErrorCode::Media.reply(error.to_string()),
    }
}
pub(crate) fn playback(operation: &str, result: anyhow::Result<String>) -> Value {
    match result {
        Ok(player_id) => Reply::Playback {
            operation,
            player_id: &player_id,
        }
        .envelope(),
        Err(error) => ErrorCode::Media.reply(error.to_string()),
    }
}
pub(crate) fn seek(operation: &str, offset_seconds: i64, result: anyhow::Result<String>) -> Value {
    match result {
        Ok(player_id) => Reply::Seek {
            operation,
            player_id: &player_id,
            offset_seconds,
        }
        .envelope(),
        Err(error) => ErrorCode::Media.reply(error.to_string()),
    }
}
pub(crate) fn workspace(request: &WorkspaceRequest, result: anyhow::Result<()>) -> Value {
    match result {
        Ok(()) => Reply::Workspace {
            operation: "workspace.focus",
            workspace_id: request.workspace_id,
            on_current_monitor: request.on_current_monitor,
        }
        .envelope(),
        Err(error) => ErrorCode::Workspace.reply(error.to_string()),
    }
}
pub(crate) fn updates(result: anyhow::Result<UpdateState>) -> Value {
    match result {
        Ok(state) => Reply::Updates { updates: &state }.envelope(),
        Err(error) => ErrorCode::Updates.reply(error.to_string()),
    }
}
