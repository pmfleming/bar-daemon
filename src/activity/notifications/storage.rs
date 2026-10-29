//! Notification operations; futures are driven only by the dedicated owner thread.
#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use turso::params;

use super::{
    center::{self, CenterPage, CenterQuery, Preview},
    database::NotificationStore,
    history::{CatalogRecord, MAX_PAGE, PAGE_BYTES, Position, SCOPE_LIMIT, search_text},
    model::{ActiveNotification, HistoryNotification},
    persistence::Mutation,
    policy::AppPolicy,
    settings::Settings,
};

// Query-dependent matches and row lifecycle are never persisted in the projection.
// Preview's hidden grouping fields are stored explicitly without changing its wire format.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredPreview {
    preview: Preview,
    group_key: String,
    repeat_key: Option<center::RepeatKey>,
}
impl StoredPreview {
    fn from_active(notification: &ActiveNotification) -> Self {
        let mut preview = Preview::from_active(notification, "");
        Self {
            group_key: std::mem::take(&mut preview.group_key),
            repeat_key: preview.repeat_key.take(),
            preview,
        }
    }
    fn into_preview(self) -> Preview {
        Preview {
            group_key: self.group_key,
            repeat_key: self.repeat_key,
            ..self.preview
        }
    }
}
const PREVIEW_BYTES: usize = 64 * 1024;

fn integer(value: u64) -> Result<i64> {
    Ok(i64::try_from(value).context("notification integer exceeds storage range")?)
}

impl NotificationStore {
    async fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .connection
            .query("SELECT value FROM notification_meta WHERE key=?1", [key])
            .await?
            .next()
            .await?
            .map(|r| r.get(0))
            .transpose()?)
    }
    async fn set_meta(&self, key: &str, value: String) -> Result<()> {
        self.connection.execute("INSERT INTO notification_meta(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value]).await?;
        Ok(())
    }
    pub(super) async fn last_id(&self) -> Result<u32> {
        self.healthy()?;
        Ok(self
            .meta("last_notification_id")
            .await?
            .map(|v| v.parse())
            .transpose()?
            .unwrap_or(0))
    }
    async fn remember_id(&self, id: u32) -> Result<()> {
        self.set_meta(
            "last_notification_id",
            self.last_id().await?.max(id).to_string(),
        )
        .await
    }
    pub(super) async fn load_dnd(&self) -> Result<(bool, Option<u64>)> {
        self.healthy()?;
        Ok(self
            .meta("dnd")
            .await?
            .map(|v| serde_json::from_str(&v))
            .transpose()?
            .unwrap_or((false, None)))
    }
    async fn load_settings(&self) -> Result<Settings> {
        let settings: Settings = self
            .meta("settings")
            .await?
            .map(|v| serde_json::from_str(&v))
            .transpose()?
            .unwrap_or_default();
        settings.validate()?;
        Ok(settings)
    }
    pub(super) async fn settings(&mut self, value: Option<Settings>) -> Result<Settings> {
        self.healthy()?;
        let Some(value) = value else {
            return self.load_settings().await;
        };
        value.validate()?;
        self.begin().await?;
        let result = async {
            self.set_meta("settings", serde_json::to_string(&value)?)
                .await?;
            self.prune_inner().await?;
            Ok(value)
        }
        .await;
        self.finish(result).await
    }
    pub(super) async fn prune(&mut self) -> Result<usize> {
        self.begin().await?;
        let result = self.prune_inner().await;
        self.finish(result).await
    }
    async fn prune_inner(&mut self) -> Result<usize> {
        #[cfg(test)]
        self.fail(super::database::Fault::Prune)?;
        let cutoff = crate::time::unix_ms()
            .saturating_sub(u64::from(self.load_settings().await?.history_days) * 86_400_000);
        let removed = self.connection.execute(
            "DELETE FROM notifications WHERE created_unix_ms < ?2 OR (closed_unix_ms IS NOT NULL
             AND history_id < COALESCE((SELECT history_id FROM notifications ORDER BY history_id DESC LIMIT 1 OFFSET ?1), 0))",
            params![i64::try_from(SCOPE_LIMIT - 1)?, integer(cutoff)?]).await.context("prune notification history")?;
        Ok(usize::try_from(removed)?)
    }
    pub(super) async fn apply(&mut self, mutation: Mutation) -> Result<()> {
        self.begin().await?;
        let result = match mutation {
            Mutation::Save(notification) => self.save_inner(&notification).await,
            Mutation::Close {
                id,
                closed_unix_ms,
                reason,
            } => self.close_inner(Some(id), closed_unix_ms, reason).await,
            Mutation::Clear {
                closed_unix_ms,
                reason,
            } => self.close_inner(None, closed_unix_ms, reason).await,
            Mutation::SetDnd {
                enabled,
                until_unix_ms,
            } => {
                self.set_meta(
                    "dnd",
                    serde_json::to_string(&(enabled, enabled.then_some(until_unix_ms).flatten()))
                        .expect("serialize DND pair"),
                )
                .await
            }
        };
        self.finish(result).await
    }
    pub(super) async fn save_once(
        &mut self,
        key: &str,
        notification: &ActiveNotification,
    ) -> Result<bool> {
        self.begin().await?;
        let result = async {
            let inserted = self
                .connection
                .execute(
                    "INSERT OR IGNORE INTO notification_once VALUES (?1,?2)",
                    params![key, integer(notification.created_unix_ms)?],
                )
                .await?
                > 0;
            if inserted {
                self.save_inner(notification).await?;
            }
            self.connection
                .execute(
                    "DELETE FROM notification_once WHERE created_unix_ms < ?1",
                    [integer(
                        notification
                            .created_unix_ms
                            .saturating_sub(180 * 86400 * 1000),
                    )?],
                )
                .await?;
            self.prune_inner().await?;
            Ok(inserted)
        }
        .await;
        self.finish(result).await
    }
    async fn save_inner(&mut self, notification: &ActiveNotification) -> Result<()> {
        self.remember_id(notification.id).await?;
        #[cfg(test)]
        self.fail(super::database::Fault::Save)?;
        if notification.hints.transient {
            self.connection
                .execute(
                    "DELETE FROM notifications WHERE session_id=?1 AND closed_unix_ms IS NULL",
                    [i64::from(notification.id)],
                )
                .await?;
            return Ok(());
        }
        let payload = serde_json::to_string(notification)?;
        ensure!(
            payload.len() <= PAGE_BYTES,
            "Notification exceeds history page byte limit"
        );
        let search = search_text(notification);
        let stored = StoredPreview::from_active(notification);
        let preview = serde_json::to_string(&stored)?;
        ensure!(
            preview.len() <= PREVIEW_BYTES,
            "notification preview exceeds storage bound"
        );
        let key = stored.preview.app_key;
        let changed = self.connection.execute(
            "UPDATE notifications SET payload_json=?2, updated_unix_ms=?3, search_text=?4, preview_json=?5, app_key=?6 WHERE session_id=?1 AND closed_unix_ms IS NULL",
            params![i64::from(notification.id), payload.clone(), integer(notification.updated_unix_ms)?, search.clone(), preview.clone(), key.clone()]).await?;
        if changed == 0 {
            self.connection.execute("INSERT INTO notifications(session_id,payload_json,created_unix_ms,updated_unix_ms,search_text,preview_json,app_key) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![i64::from(notification.id), payload, integer(notification.created_unix_ms)?, integer(notification.updated_unix_ms)?, search, preview, key]).await?;
        }
        Ok(())
    }
    async fn close_inner(&self, id: Option<u32>, closed: u64, reason: u32) -> Result<()> {
        self.connection.execute("UPDATE notifications SET closed_unix_ms=?1, close_reason=?2 WHERE closed_unix_ms IS NULL AND (?3 IS NULL OR session_id=?3)",
            params![integer(closed)?, i64::from(reason), id.map(i64::from)]).await?;
        Ok(())
    }
    pub(super) async fn delete(&mut self, positions: &[Position]) -> Result<()> {
        self.begin().await?;
        let result = async {
            let mut statement = self
                .connection
                .prepare("DELETE FROM notifications WHERE session_id=?1 AND created_unix_ms=?2")
                .await?;
            for position in positions {
                statement
                    .execute(params![i64::from(position.id), integer(position.created)?])
                    .await?;
            }
            Ok(())
        }
        .await;
        self.finish(result).await
    }
    pub(super) async fn set_policy(&mut self, key: &str, policy: &AppPolicy) -> Result<()> {
        self.begin().await?;
        let result = self
            .set_meta(&format!("app_policy:{key}"), serde_json::to_string(policy)?)
            .await;
        self.finish(result).await
    }
    pub(super) async fn policies(&self) -> Result<BTreeMap<String, AppPolicy>> {
        self.healthy()?;
        let mut rows = self
            .connection
            .query(
                "SELECT key,value FROM notification_meta WHERE key LIKE 'app_policy:%'",
                (),
            )
            .await?;
        let mut policies = BTreeMap::new();
        while let Some(row) = rows.next().await? {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            policies.insert(key[11..].to_owned(), serde_json::from_str(&value)?);
        }
        Ok(policies)
    }
    pub(super) async fn targets(
        &self,
        app_key: Option<&str>,
        selected: Option<Position>,
    ) -> Result<Vec<Position>> {
        self.healthy()?;
        // This deliberately covers all retained rows, not just the search window.
        let mut rows = self.connection.query(
            "SELECT session_id,created_unix_ms FROM notifications WHERE (?1 IS NULL OR app_key=?1) AND (?2 IS NULL OR (session_id=?2 AND created_unix_ms=?3)) LIMIT 100001",
            params![app_key, selected.map(|p| i64::from(p.id)), selected.map(|p| integer(p.created)).transpose()?]).await?;
        let mut targets = Vec::new();
        while let Some(row) = rows.next().await? {
            ensure!(
                targets.len() < 100_000,
                "Deletion exceeds 100,000-record safety limit"
            );
            targets.push(Position {
                id: u32::try_from(row.get::<i64>(0)?)?,
                created: u64::try_from(row.get::<i64>(1)?)?,
            });
        }
        Ok(targets)
    }
    async fn candidates(&self, query: &str) -> Result<Vec<Preview>> {
        self.healthy()?;
        // Empty searches do not even load the potentially large normalized body.
        let sql = if query.is_empty() {
            "SELECT history_id,preview_json,closed_unix_ms,'' FROM notifications ORDER BY history_id DESC LIMIT ?1"
        } else {
            "SELECT history_id,preview_json,closed_unix_ms,search_text FROM notifications ORDER BY history_id DESC LIMIT ?1"
        };
        let mut rows = self
            .connection
            .query(sql, [i64::try_from(SCOPE_LIMIT)?])
            .await?;
        let mut previews = Vec::new();
        while let Some(row) = rows.next().await? {
            let encoded: String = row.get(1)?;
            ensure!(
                encoded.len() <= PREVIEW_BYTES,
                "notification preview exceeds storage bound"
            );
            let mut preview = serde_json::from_str::<StoredPreview>(&encoded)?.into_preview();
            preview.history_id = Some(row.get(0)?);
            preview.closed_unix_ms = row.get::<Option<i64>>(2)?.map(u64::try_from).transpose()?;
            preview.matches = query.is_empty() || row.get::<String>(3)?.contains(query);
            previews.push(preview);
        }
        Ok(previews)
    }
    async fn record(&self, history_id: i64) -> Result<CatalogRecord> {
        let row = self.connection.query("SELECT history_id,payload_json,closed_unix_ms,close_reason FROM notifications WHERE history_id=?1", [history_id]).await?.next().await?.context("notification no longer retained")?;
        catalog_record(&row)
    }
    pub(super) async fn center(
        &self,
        query: &CenterQuery,
        active: &[ActiveNotification],
        epoch: &str,
        revision: u64,
    ) -> Result<CenterPage> {
        query.validate_revision(epoch, revision)?;
        let mut previews = self.candidates(&query.query).await?;
        previews.retain(|p| {
            !active
                .iter()
                .any(|n| n.id == p.id && n.created_unix_ms == p.created_unix_ms)
        });
        previews.extend(active.iter().map(|n| Preview::from_active(n, &query.query)));
        // At most one detail is fetched before the synchronous pure projection.
        // The serialized owner cannot interleave a mutation with either phase.
        let selected = if query.view == "app" {
            previews.iter().find(|p| {
                p.matches
                    && query
                        .selected
                        .is_some_and(|s| s.id == p.id && s.created == p.created_unix_ms)
                    && query.app_key.as_ref().is_none_or(|key| key == &p.app_key)
            })
        } else {
            None
        };
        let detail = if let Some(preview) = selected {
            Some(
                if let Some(n) = active
                    .iter()
                    .find(|n| n.id == preview.id && n.created_unix_ms == preview.created_unix_ms)
                {
                    n.clone().into()
                } else {
                    self.record(
                        preview
                            .history_id
                            .context("missing selected history identity")?,
                    )
                    .await?
                },
            )
        } else {
            None
        };
        center::project(previews, query, epoch, revision, |_| {
            detail.context("notification no longer retained")
        })
        .map_err(anyhow::Error::new)
    }
    pub(super) async fn query(
        &self,
        query: &str,
        before: Option<Position>,
        limit: usize,
        excluded: &[(u64, u32)],
    ) -> Result<Vec<CatalogRecord>> {
        ensure!(
            (1..=MAX_PAGE).contains(&limit),
            "invalid notification page limit"
        );
        let mut previews = self.candidates(query).await?;
        previews.retain(|p| {
            p.matches
                && before
                    .is_none_or(|before| (p.created_unix_ms, p.id) < (before.created, before.id))
                && !excluded.contains(&(p.created_unix_ms, p.id))
        });
        previews.sort_by_key(|p| std::cmp::Reverse((p.created_unix_ms, p.id)));
        let mut records = Vec::new();
        let mut bytes = 0;
        for preview in previews.into_iter().take(limit + 1) {
            let record = self
                .record(preview.history_id.context("missing history identity")?)
                .await?;
            bytes += serde_json::to_vec(&record)?.len();
            records.push(record);
            // Include one overflow row so history::page can produce its cursor.
            if bytes > PAGE_BYTES {
                break;
            }
        }
        Ok(records)
    }
    pub(super) async fn list(
        &self,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<HistoryNotification>> {
        self.healthy()?;
        let mut rows = self.connection.query("SELECT history_id,payload_json,closed_unix_ms,close_reason FROM notifications WHERE history_id < ?1 ORDER BY history_id DESC LIMIT ?2",
            params![before.unwrap_or(i64::MAX), i64::try_from(limit)?]).await?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().await? {
            records.push(history_record(&row)?);
        }
        Ok(records)
    }
}

fn history_record(row: &turso::Row) -> Result<HistoryNotification> {
    let payload: String = row.get(1)?;
    ensure!(
        payload.len() <= PAGE_BYTES,
        "Notification exceeds history page byte limit"
    );
    Ok(HistoryNotification {
        history_id: row.get(0)?,
        notification: serde_json::from_str(&payload)?,
        closed_unix_ms: row.get::<Option<i64>>(2)?.map(u64::try_from).transpose()?,
        close_reason: row.get::<Option<i64>>(3)?.map(u32::try_from).transpose()?,
    })
}
fn catalog_record(row: &turso::Row) -> Result<CatalogRecord> {
    let record = history_record(row)?;
    Ok(CatalogRecord {
        history_id: Some(record.history_id),
        notification: record.notification,
        closed_unix_ms: record.closed_unix_ms,
        close_reason: record.close_reason,
    })
}
