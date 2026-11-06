//! One persisted-receipt validation contract for startup, lookup and restore.
use super::model::{MutationResult, identifier};
use anyhow::{Result, ensure};

pub(super) fn decode(
    request: &str,
    fingerprint: &str,
    body: &[u8],
    catalog_revision: u64,
) -> Result<MutationResult> {
    identifier(request)?;
    ensure!(
        fingerprint.len() == 64 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid receipt fingerprint"
    );
    let result: MutationResult = serde_json::from_slice(body)?;
    ensure!(
        result.request_id == request
            && result.catalog_revision > 0
            && result.catalog_revision <= catalog_revision
            && (result.item.is_some() != result.deleted_id.is_some()),
        "invalid receipt identity/revision/outcome"
    );
    if let Some(item) = &result.item {
        item.validate()?;
    }
    if let Some(id) = &result.deleted_id {
        identifier(id)?;
    }
    Ok(result)
}
