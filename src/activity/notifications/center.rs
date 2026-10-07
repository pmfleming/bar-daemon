//! Bounded native app aggregation and five-row notification index. This reads
//! metadata, not 5,000 bodies, and never changes notification lifecycle state.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    history::{CatalogRecord, HistoryError, HistoryQuery, PAGE_BYTES, Position},
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
}
const fn first_page() -> usize {
    1
}
impl CenterQuery {
    pub fn normalize(&mut self) -> Result<(), HistoryError> {
        if !["apps", "app"].contains(&self.view.as_str())
            || self.offset > 5200
            || self.page == 0
            || self.page > 1040
            || self.app_key.as_ref().is_some_and(|s| s.len() > 4096)
            || self.app_anchor.as_ref().is_some_and(|s| s.len() > 4096)
            || self.group_key.as_ref().is_some_and(|s| s.len() > 4096)
            || self.selected.is_some_and(|p| p.id == 0 || p.created == 0)
        {
            return Err(HistoryError::Invalid);
        }
        let mut query = HistoryQuery {
            query: self.query.clone(),
            cursor: None,
            anchor: self.page_anchor,
            limit: INDEX_SIZE,
        };
        query.normalize()?;
        self.query = query.query;
        Ok(())
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
}
impl Preview {
    pub fn from_active(n: &ActiveNotification, query: &str) -> Self {
        Self {
            id: n.id,
            created_unix_ms: n.created_unix_ms,
            app_key: app_key(&n.hints.desktop_entry, &n.app_name, n.id, n.created_unix_ms),
            app_name: clip(&n.app_name, 128),
            app_icon: clip(&n.app_icon, 512),
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
        }
    }
    fn position(&self) -> (u64, u32) {
        (self.created_unix_ms, self.id)
    }
}
pub(crate) fn clip(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}
// Descriptive grouping, not authentication. Never route a mutation by app key.
// Unnamed senders do not all collapse into a shared "unknown" app.
pub(crate) fn app_key(desktop: &str, name: &str, id: u32, created: u64) -> String {
    let unknown = || format!("unknown:{created}:{id}");
    if desktop.len() > 1024 {
        unknown()
    } else if !desktop.trim().is_empty() {
        format!("desktop:{}", desktop.trim())
    } else if name.len() > 1024 || name.trim().is_empty() {
        unknown()
    } else {
        // Notification artwork represents content/urgency, not sender identity.
        // Keep known desktop IDs distinct even when their display names match.
        format!("named:{}", name.trim())
    }
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
    mut lookup: impl FnMut(&Preview) -> anyhow::Result<CatalogRecord>,
) -> Result<CenterPage, HistoryError> {
    query.validate_revision(epoch, revision)?;
    rows.sort_by_key(|p| std::cmp::Reverse(p.position()));
    let mut totals = BTreeMap::<String, usize>::new();
    for row in &rows {
        *totals.entry(row.app_key.clone()).or_default() += 1;
    }
    if query.view == "apps" {
        let mut apps = Vec::<AppSummary>::new();
        let mut indices = BTreeMap::<String, usize>::new();
        for row in rows.into_iter().filter(|p| p.matches) {
            if let Some(&index) = indices.get(&row.app_key) {
                apps[index].count += 1;
            } else {
                indices.insert(row.app_key.clone(), apps.len());
                apps.push(AppSummary {
                    key: row.app_key.clone(),
                    count: 1,
                    total_count: totals[&row.app_key],
                    latest: row,
                });
            }
        }
        let total_apps = apps.len();
        let end = (query.offset + APP_PAGE_SIZE).min(total_apps);
        let anchor_reached = query.app_anchor.as_ref().is_none_or(|key| {
            apps.iter()
                .position(|app| &app.key == key)
                .is_none_or(|index| index < end)
        });
        let apps = apps
            .into_iter()
            .skip(query.offset)
            .take(APP_PAGE_SIZE)
            .collect();
        return checked(CenterPage::Apps {
            epoch: epoch.into(),
            revision: revision.to_string(),
            query: query.query.clone(),
            apps,
            total_apps,
            offset: query.offset,
            next_offset: (end < total_apps).then_some(end),
            anchor_reached,
        });
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
    let total_count = totals.get(&key).copied().unwrap_or(0);
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|p| p.app_key == key && p.matches)
        .collect();
    let count = rows.len();
    let pages = count.div_ceil(INDEX_SIZE).max(1);
    let page = query
        .page_anchor
        .and_then(|a| rows.iter().position(|p| p.position() == (a.created, a.id)))
        .map_or(query.page, |index| index / INDEX_SIZE + 1)
        .min(pages);
    let overview = rows.iter().take(3).cloned().collect();
    let entries = rows
        .iter()
        .skip((page - 1) * INDEX_SIZE)
        .take(INDEX_SIZE)
        .cloned()
        .collect();
    let selected = rows
        .iter()
        .find(|p| {
            query
                .selected
                .is_some_and(|s| p.position() == (s.created, s.id))
        })
        .map(&mut lookup)
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
        overview,
        entries,
        selected,
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
