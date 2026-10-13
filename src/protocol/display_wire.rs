//! v1 display control DTOs. Layout/focus decoding errors retain their domain codes.
//! Keep legacy DTO names: Serde uses them in malformed-request messages.
use crate::{api, display_policy::DisplayPolicyState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Token {
    pub id: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Patch {
    pub values: BTreeMap<String, Value>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Empty {}

#[derive(Serialize)]
struct Reply<'a> {
    display_policy: &'a DisplayPolicyState,
}
#[derive(Clone, Copy)]
pub(crate) enum Failure {
    Layout,
    Focus,
    Policy,
}
impl Failure {
    pub(crate) fn reply(self, error: &anyhow::Error) -> Value {
        api::error(
            match self {
                Self::Layout => "display-layout-failed",
                Self::Focus => "display-focus-failed",
                Self::Policy => "display-policy-failed",
            },
            format!("{error:#}"),
        )
    }
}
pub(crate) fn reply(result: anyhow::Result<DisplayPolicyState>, failure: Failure) -> Value {
    match result {
        Ok(state) => api::success(
            serde_json::to_value(Reply {
                display_policy: &state,
            })
            .expect("serializable display reply"),
        ),
        Err(error) => failure.reply(&error),
    }
}
