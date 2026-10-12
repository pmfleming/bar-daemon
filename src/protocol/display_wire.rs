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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn moved_request_types_keep_legacy_deserialization_error_names() {
        for (error, name) in [
            (
                serde_json::from_value::<Token>(Value::Null).err().unwrap(),
                "Token",
            ),
            (
                serde_json::from_value::<Patch>(Value::Null).err().unwrap(),
                "Patch",
            ),
            (
                serde_json::from_value::<Empty>(Value::Null).err().unwrap(),
                "Empty",
            ),
        ] {
            assert_eq!(
                error.to_string(),
                format!("invalid type: null, expected struct {name}")
            );
        }
    }

    #[test]
    fn replies_preserve_normalized_outputs_and_v1_error_envelopes() {
        let state = crate::display_policy::contract_examples::states()["initial"].clone();
        let response = reply(Ok(state.clone()), Failure::Layout);
        assert_eq!(response, api::success(json!({"display_policy": state})));
        let output = &response["data"]["display_policy"]["outputs"][0];
        assert_eq!(output["supported"], true);
        assert_eq!(output["internal"], true);
        assert_eq!(output["current_mode"], "1920x1200@60.00Hz");
        assert_eq!(output["modes"][0]["id"], output["current_mode"]);
        for (failure, code) in [
            (Failure::Layout, "display-layout-failed"),
            (Failure::Focus, "display-focus-failed"),
            (Failure::Policy, "display-policy-failed"),
        ] {
            let response = reply(Err(anyhow::anyhow!("cause").context("context")), failure);
            assert_eq!(response, api::error(code, "context: cause"));
            assert!(response.get("data").is_none());
            assert!(response["error"].get("retryable").is_none());
        }
    }
}
