//! Shared serializers for production workspace replies and offline fixtures.
use super::{CloseRecord, Context, Lease, Reservation};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Serialize, JsonSchema)]
pub(super) struct ContextReply {
    pub version: u32,
    pub context: Context,
    pub request_id: String,
}

#[derive(Serialize, JsonSchema)]
pub(super) struct LeaseReply {
    pub status: String,
    pub lease: Lease,
}

#[derive(Serialize, JsonSchema)]
pub(super) struct FocusReply {
    pub status: String,
    pub workspace_id: i64,
}

#[derive(Serialize, JsonSchema)]
#[serde(untagged)]
pub(super) enum RecordedReply<'a> {
    Reservation(&'a Reservation),
    Close(&'a CloseRecord),
}

#[derive(Serialize, JsonSchema)]
#[serde(untagged)]
pub(super) enum CommandReply<'a> {
    Recorded(RecordedReply<'a>),
    Focus(FocusReply),
}
