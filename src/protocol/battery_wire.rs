//! Battery v1 requests and outcomes; hardware and persistence stay in the battery domain.
use crate::{
    battery::helper::ThresholdWriteResult,
    model::{BatteryHistoryState, BatteryProfileAction, BatteryState},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Serialize)]
pub(crate) struct BatteryRequest {
    pub battery_id: Option<String>,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct ThresholdRequest {
    pub battery_id: String,
    pub start_percent: u8,
    pub end_percent: u8,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct ProtectionRequest {
    pub battery_id: Option<String>,
    pub enabled: bool,
    pub start_percent: Option<u8>,
    pub end_percent: Option<u8>,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct InhibitionRequest {
    pub battery_id: Option<String>,
    pub enabled: bool,
}
#[derive(Default, PartialEq, Deserialize, Serialize)]
pub(crate) struct AlertPolicyRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_when_full: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_power_saver: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_warning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_critical: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_profile: Option<BatteryProfileAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical_profile: Option<BatteryProfileAction>,
}
#[derive(Clone, Copy)]
pub(crate) enum ErrorCode {
    ConfigFailed,
    NotPlugged,
    OperationActive,
    OperationFailed,
    OperationInactive,
    OperationUnsupported,
    ProtectionUnsupported,
    RefreshFailed,
    StateFailed,
    Unavailable,
    ValidationError,
}
pub(crate) fn error(code: ErrorCode, message: impl Into<String>) -> Value {
    super::error(
        match code {
            ErrorCode::ConfigFailed => "battery-config-failed",
            ErrorCode::NotPlugged => "battery-not-plugged",
            ErrorCode::OperationActive => "battery-operation-active",
            ErrorCode::OperationFailed => "battery-operation-failed",
            ErrorCode::OperationInactive => "battery-operation-inactive",
            ErrorCode::OperationUnsupported => "battery-operation-unsupported",
            ErrorCode::ProtectionUnsupported => "battery-protection-unsupported",
            ErrorCode::RefreshFailed => "battery-refresh-failed",
            ErrorCode::StateFailed => "battery-state-failed",
            ErrorCode::Unavailable => "battery-unavailable",
            ErrorCode::ValidationError => "validation-error",
        },
        message,
    )
}
#[derive(Serialize)]
struct ThresholdOutcome<'a> {
    battery_id: &'a str,
    start_percent: u8,
    end_percent: u8,
    verified: bool,
}
#[derive(Serialize)]
struct BatteryReply<'a> {
    battery: BatteryState,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<ThresholdOutcome<'a>>,
}
pub(crate) fn response(
    result: anyhow::Result<BatteryState>,
    operation: Option<(&str, ThresholdWriteResult)>,
) -> Value {
    match result {
        Ok(battery) => super::serialized_success(BatteryReply {
            battery,
            operation: operation.map(|(battery_id, result)| ThresholdOutcome {
                battery_id,
                start_percent: result.actual_start_percent,
                end_percent: result.actual_end_percent,
                verified: result.verified,
            }),
        }),
        Err(value) => error(ErrorCode::RefreshFailed, value.to_string()),
    }
}
pub(crate) fn history(history: BatteryHistoryState) -> Value {
    #[derive(Serialize)]
    struct HistoryReply {
        history: BatteryHistoryState,
    }
    super::serialized_success(HistoryReply { history })
}
