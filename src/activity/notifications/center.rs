//! Bounded native app aggregation and five-row notification index. This reads
//! metadata, not 5,000 bodies, and never changes notification lifecycle state.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    history::{CatalogRecord, HistoryError, PAGE_BYTES, Position, normalize_search},
    model::ActiveNotification,
};

pub(crate) const INDEX_SIZE: usize = 5;
pub(crate) const APP_PAGE_SIZE: usize = 50;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CenterQuery {
    pub view: String,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub offset: usize,
    pub epoch: Option<String>,
    pub revision: Option<String>,
    pub app_anchor: Option<String>,
    pub app_key: Option<String>,
    #[serde(default = "first_page")]
    pub page: usize,
    pub page_anchor: Option<Position>,
    pub selected: Option<Position>,
    pub group_key: Option<String>,
    pub date: Option<String>,
    #[serde(default)]
    pub period_groups: bool,
    pub period_day: Option<String>,
    pub timeline_anchor: Option<String>,
    #[serde(default = "default_grouping")]
    pub group_similar: bool,
}
const fn default_grouping() -> bool {
    true
}
const fn first_page() -> usize {
    1
}
impl CenterQuery {
    pub fn normalize(&mut self) -> Result<(), HistoryError> {
        if !["apps", "app", "timeline"].contains(&self.view.as_str())
            || self.offset > 5200
            || self.date.as_ref().is_some_and(|s| s.len() > 10)
            || self.period_day.as_ref().is_some_and(|s| s.len() > 10)
            || (self.period_groups
                && self
                    .date
                    .as_ref()
                    .is_some_and(|s| !["today", "week", "month", "older"].contains(&s.as_str())))
            || self.timeline_anchor.as_ref().is_some_and(|s| s.len() > 64)
            || self.page == 0
            || self.page > 1040
            || [&self.app_key, &self.app_anchor, &self.group_key]
                .into_iter()
                .flatten()
                .any(|s| s.len() > 4096)
            || self.selected.is_some_and(|p| p.id == 0 || p.created == 0)
        {
            return Err(HistoryError::Invalid);
        }
        if self.page_anchor.is_some_and(Position::invalid) {
            return Err(HistoryError::Invalid);
        }
        normalize_search(&mut self.query)
    }
    pub fn validate_revision(&self, epoch: &str, revision: u64) -> Result<(), HistoryError> {
        if self.offset > 0
            && (self.epoch.as_deref() != Some(epoch)
                || self.revision.as_deref() != Some(revision.to_string().as_str()))
        {
            return Err(HistoryError::Stale);
        }
        Ok(())
    }
}

// Only artwork metadata is needed by compact views, not sound/action hints.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PreviewHints {
    pub desktop_entry: String,
    pub image_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Preview {
    pub id: u32,
    pub created_unix_ms: u64,
    pub app_key: String,
    pub app_name: String,
    pub app_icon: String,
    pub identity_icon: String,
    pub hints: PreviewHints,
    pub summary: String,
    pub body: String,
    pub closed_unix_ms: Option<u64>,
    pub snoozed_until_unix_ms: Option<u64>,
    #[serde(skip)]
    pub history_id: Option<i64>,
    #[serde(skip)]
    pub matches: bool,
    #[serde(skip)]
    pub group_key: String,
    #[serde(skip)]
    pub repeat_key: Option<RepeatKey>,
}
impl Preview {
    pub fn from_active(n: &ActiveNotification, query: &str) -> Self {
        Self {
            id: n.id,
            created_unix_ms: n.created_unix_ms,
            app_key: n.app_key(),
            app_name: clip(&n.app_name, 128),
            app_icon: clip(&n.app_icon, 512),
            identity_icon: clip(&n.identity_icon, 1024),
            hints: PreviewHints {
                desktop_entry: clip(&n.hints.desktop_entry, 1024),
                image_path: clip(&n.hints.image_path, 512),
            },
            summary: clip(&n.summary, 160),
            body: clip(&n.body, 240),
            closed_unix_ms: None,
            snoozed_until_unix_ms: n.snoozed_until_unix_ms,
            history_id: None,
            matches: super::history::search_text(n).contains(query),
            group_key: clip(&n.group_key, 4097),
            repeat_key: repeat_key(
                &n.summary,
                &n.body,
                &n.hints.category,
                n.hints.urgency,
                n.actions.is_empty(),
            ),
        }
    }
    fn position(&self) -> (u64, u32) {
        (self.created_unix_ms, self.id)
    }
}
pub(crate) fn clip(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}
// Structural equality avoids serialization and cannot confuse embedded separators.
pub(super) type RepeatKey = (String, String, String, u8);
pub(super) fn repeat_key(
    summary: &str,
    body: &str,
    category: &str,
    urgency: u8,
    action_free: bool,
) -> Option<RepeatKey> {
    (action_free
        && summary.chars().count() < 160
        && body.chars().count() < 240
        && category.chars().count() <= 256)
        .then(|| (summary.into(), body.into(), category.into(), urgency))
}

#[derive(Debug, Serialize)]
pub(crate) struct AppSummary {
    pub key: String,
    pub count: usize,
    pub total_count: usize,
    pub latest: Preview,
}
#[derive(Debug, Serialize)]
#[serde(tag = "view")]
pub(crate) enum CenterPage {
    #[serde(rename = "timeline")]
    Timeline(super::timeline::Timeline),
    #[serde(rename = "apps")]
    Apps {
        epoch: String,
        revision: String,
        query: String,
        apps: Vec<AppSummary>,
        total_apps: usize,
        offset: usize,
        next_offset: Option<usize>,
        anchor_reached: bool,
    },
    #[serde(rename = "app")]
    App {
        epoch: String,
        revision: String,
        query: String,
        app_key: String,
        count: usize,
        total_count: usize,
        page: usize,
        pages: usize,
        overview: Vec<Preview>,
        entries: Vec<Preview>,
        selected: Option<Box<CatalogRecord>>,
    },
}

pub(crate) fn project(
    mut rows: Vec<Preview>,
    query: &CenterQuery,
    epoch: &str,
    revision: u64,
    lookup: impl FnOnce(&Preview) -> anyhow::Result<CatalogRecord>,
) -> Result<CenterPage, HistoryError> {
    query.validate_revision(epoch, revision)?;
    rows.sort_by_key(|p| std::cmp::Reverse(p.position()));
    if query.view == "apps" {
        return project_apps(rows, query, epoch, revision);
    }
    let key = query
        .app_key
        .clone()
        .or_else(|| {
            rows.iter()
                .find(|p| {
                    query
                        .selected
                        .is_some_and(|s| p.position() == (s.created, s.id))
                        || query.group_key.as_ref().is_some_and(|g| &p.group_key == g)
                })
                .map(|p| p.app_key.clone())
        })
        .unwrap_or_default();
    rows.retain(|p| p.app_key == key);
    let total_count = rows.len();
    rows.retain(|p| p.matches);
    if query.view == "timeline" {
        return checked(CenterPage::Timeline(super::timeline::project(
            rows, query, epoch, revision,
        )?));
    }
    let count = rows.len();
    let pages = count.div_ceil(INDEX_SIZE).max(1);
    let page = query
        .page_anchor
        .and_then(|a| rows.iter().position(|p| p.position() == (a.created, a.id)))
        .map_or(query.page, |index| index / INDEX_SIZE + 1)
        .min(pages);
    let selected = query
        .selected
        .and_then(|s| rows.iter().find(|p| p.position() == (s.created, s.id)))
        .map(lookup)
        .transpose()
        .map_err(HistoryError::Unavailable)?
        .map(Box::new);
    checked(CenterPage::App {
        epoch: epoch.into(),
        revision: revision.to_string(),
        query: query.query.clone(),
        app_key: key,
        count,
        total_count,
        page,
        pages,
        overview: rows.iter().take(3).cloned().collect(),
        entries: rows
            .into_iter()
            .skip((page - 1) * INDEX_SIZE)
            .take(INDEX_SIZE)
            .collect(),
        selected,
    })
}
fn project_apps(
    rows: Vec<Preview>,
    query: &CenterQuery,
    epoch: &str,
    revision: u64,
) -> Result<CenterPage, HistoryError> {
    let mut groups = BTreeMap::<String, Vec<Preview>>::new();
    for row in rows {
        groups.entry(row.app_key.clone()).or_default().push(row);
    }
    let mut apps: Vec<_> = groups
        .into_iter()
        .filter_map(|(key, rows)| {
            let total_count = rows.len();
            let mut matching = rows.into_iter().filter(|p| p.matches);
            let latest = matching.next()?;
            Some(AppSummary {
                key,
                count: 1 + matching.count(),
                total_count,
                latest,
            })
        })
        .collect();
    apps.sort_by_key(|app| std::cmp::Reverse(app.latest.position()));
    let total_apps = apps.len();
    let end = (query.offset + APP_PAGE_SIZE).min(total_apps);
    let anchor_reached = query.app_anchor.as_ref().is_none_or(|key| {
        apps.iter()
            .position(|app| &app.key == key)
            .is_none_or(|index| index < end)
    });
    checked(CenterPage::Apps {
        epoch: epoch.into(),
        revision: revision.to_string(),
        query: query.query.clone(),
        apps: apps
            .into_iter()
            .skip(query.offset)
            .take(APP_PAGE_SIZE)
            .collect(),
        total_apps,
        offset: query.offset,
        next_offset: (end < total_apps).then_some(end),
        anchor_reached,
    })
}
fn checked(page: CenterPage) -> Result<CenterPage, HistoryError> {
    if serde_json::to_vec(&page)
        .map_err(|e| HistoryError::Unavailable(e.into()))?
        .len()
        > PAGE_BYTES
    {
        return Err(HistoryError::Unavailable(anyhow::anyhow!(
            "Notification center page exceeds byte limit"
        )));
    }
    Ok(page)
}
