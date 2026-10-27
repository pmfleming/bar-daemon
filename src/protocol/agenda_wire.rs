//! Agenda v1 requests use the real bar success/error envelopes.
use crate::activity::agenda_store::model::{ItemPatch, NewItem};
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Get {
    pub id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub request_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Create {
    pub request_id: String,
    pub item: NewItem,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Patch {
    pub request_id: String,
    pub id: String,
    pub expected_revision: u64,
    pub patch: ItemPatch,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Delete {
    pub request_id: String,
    pub id: String,
    pub expected_revision: u64,
}
