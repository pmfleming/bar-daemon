//! Read-only notification catalog. Cursors retain no server-side leases and
//! are invalidated by every content mutation or daemon replacement.
use std::io::Read;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::model::ActiveNotification;

pub(crate) const SCOPE_LIMIT: usize = 5_000;
pub(crate) const MAX_PAGE: usize = 100;
pub(crate) const PAGE_BYTES: usize = 512 * 1024;
const CURSOR_TTL_MS: u64 = 120_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryQuery {
    #[serde(default)]
    pub query: String,
    pub cursor: Option<String>,
    /// Oldest visible result to preserve across an authoritative refresh.
    pub anchor: Option<Position>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}
const fn default_limit() -> usize {
    50
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct Position {
    pub created: u64,
    pub id: u32,
}
impl Position {
    pub fn contains(self, notification: &ActiveNotification) -> bool {
        (notification.created_unix_ms, notification.id) < (self.created, self.id)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    epoch: String,
    revision: String,
    query: String,
    before: Position,
    expires: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CatalogRecord {
    pub history_id: Option<i64>,
    pub notification: ActiveNotification,
    pub closed_unix_ms: Option<u64>,
    pub close_reason: Option<u32>,
}
impl From<ActiveNotification> for CatalogRecord {
    fn from(notification: ActiveNotification) -> Self {
        Self {
            history_id: None,
            notification,
            closed_unix_ms: None,
            close_reason: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct HistoryPage {
    pub epoch: String,
    // Strings preserve exact u64 revisions in JavaScript clients.
    pub revision: String,
    pub query: String,
    pub records: Vec<CatalogRecord>,
    pub next_cursor: Option<String>,
    pub scope_limit: usize,
    pub anchor_reached: bool,
}

#[derive(Debug)]
pub(crate) enum HistoryError {
    Invalid,
    Stale,
    Busy,
    Unavailable(anyhow::Error),
}
impl std::fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}
impl std::error::Error for HistoryError {}
impl HistoryError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "history-query-invalid",
            Self::Stale => "history-cursor-stale",
            Self::Busy => "history-busy",
            Self::Unavailable(_) => "history-unavailable",
        }
    }
    pub fn message(&self) -> String {
        match self {
            Self::Invalid => "Invalid notification query, page size or cursor".into(),
            Self::Stale => {
                "Notification history changed or cursor expired; refresh the query".into()
            }
            Self::Busy => "Notification history is busy; retry the read".into(),
            Self::Unavailable(error) => error.to_string(),
        }
    }
}

pub(crate) fn new_epoch() -> Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?
        .read_exact(&mut bytes)
        .context("notification history epoch")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

impl HistoryQuery {
    pub fn normalize(&mut self) -> Result<(), HistoryError> {
        if self.query.len() > 1024
            || !(1..=MAX_PAGE).contains(&self.limit)
            || self
                .anchor
                .is_some_and(|anchor| anchor.id == 0 || anchor.created > i64::MAX as u64)
        {
            return Err(HistoryError::Invalid);
        }
        self.query = self.query.trim().to_lowercase();
        if self.query.len() > 1024 {
            return Err(HistoryError::Invalid);
        }
        Ok(())
    }
    pub fn position(
        &self,
        epoch: &str,
        revision: u64,
        now: u64,
    ) -> Result<Option<Position>, HistoryError> {
        let Some(token) = &self.cursor else {
            return Ok(None);
        };
        // A valid 1,024-byte query can expand sixfold when JSON-escaped.
        if token.len() > 8192 {
            return Err(HistoryError::Invalid);
        }
        let cursor: Cursor = serde_json::from_str(token).map_err(|_| HistoryError::Invalid)?;
        if cursor.epoch != epoch
            || cursor.revision != revision.to_string()
            || cursor.query != self.query
            || cursor.expires <= now
            || cursor.expires > now.saturating_add(CURSOR_TTL_MS)
        {
            return Err(HistoryError::Stale);
        }
        if cursor.before.id == 0 || cursor.before.created > i64::MAX as u64 {
            return Err(HistoryError::Invalid);
        }
        Ok(Some(cursor.before))
    }
}

pub(crate) fn search_text(notification: &ActiveNotification) -> String {
    format!(
        "{} {} {}",
        notification.app_name, notification.summary, notification.body
    )
    .to_lowercase()
}

// Persistence supplies at most limit+1 matching non-live rows. Live rows are
// bounded by ingress policy. No UI must merge, classify or sort this catalog.
pub(crate) fn page(
    mut persisted: Vec<CatalogRecord>,
    active: Vec<ActiveNotification>,
    query: &HistoryQuery,
    before: Option<Position>,
    epoch: &str,
    revision: u64,
    now: u64,
) -> Result<HistoryPage, HistoryError> {
    persisted.extend(
        active
            .into_iter()
            .filter(|item| {
                before.is_none_or(|position| position.contains(item))
                    && search_text(item).contains(&query.query)
            })
            .map(CatalogRecord::from),
    );
    persisted.sort_by(|a, b| {
        (b.notification.created_unix_ms, b.notification.id)
            .cmp(&(a.notification.created_unix_ms, a.notification.id))
    });
    // Popup lifecycle does not affect catalog contents or revisions.
    for record in &mut persisted {
        record.notification.toast_visible = false;
        record.notification.toast_expires_unix_ms = None;
    }
    let mut records = Vec::new();
    let mut bytes = 0;
    let mut more = false;
    for record in persisted {
        let size = serde_json::to_vec(&record)
            .map_err(|e| HistoryError::Unavailable(e.into()))?
            .len();
        if records.len() == query.limit || bytes + size > PAGE_BYTES {
            if records.is_empty() {
                return Err(HistoryError::Unavailable(anyhow::anyhow!(
                    "Notification exceeds history page byte limit"
                )));
            }
            more = true;
            break;
        }
        bytes += size;
        records.push(record);
    }
    let next_cursor = records
        .last()
        .filter(|_| more)
        .map(|record| {
            serde_json::to_string(&Cursor {
                epoch: epoch.into(),
                revision: revision.to_string(),
                query: query.query.clone(),
                before: Position {
                    created: record.notification.created_unix_ms,
                    id: record.notification.id,
                },
                expires: now.saturating_add(CURSOR_TTL_MS),
            })
        })
        .transpose()
        .map_err(|e| HistoryError::Unavailable(e.into()))?;
    let anchor_reached = !more
        || query.anchor.is_none_or(|anchor| {
            records.last().is_some_and(|record| {
                (record.notification.created_unix_ms, record.notification.id)
                    <= (anchor.created, anchor.id)
            })
        });
    Ok(HistoryPage {
        epoch: epoch.into(),
        revision: revision.to_string(),
        query: query.query.clone(),
        records,
        next_cursor,
        scope_limit: SCOPE_LIMIT,
        anchor_reached,
    })
}
