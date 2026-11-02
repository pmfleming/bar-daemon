//! Notification operations; futures are driven only by the dedicated owner thread.
#[path = "storage_rows.rs"]
mod rows;
#[path = "storage_state.rs"]
mod state;
#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "storage_v2_tests.rs"]
mod v2_tests;
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

fn integer(value: u64) -> Result<i64> {
    i64::try_from(value).context("notification integer exceeds storage range")
}

impl NotificationStore {
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
        self.delete_where(
            "created_unix_ms < ?2 OR (closed_unix_ms IS NOT NULL
             AND history_id < COALESCE((SELECT history_id FROM notifications ORDER BY history_id DESC LIMIT 1 OFFSET ?1), 0))",
            vec![i64::try_from(SCOPE_LIMIT - 1)?.into(), integer(cutoff)?.into()]).await.context("prune notification history")
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
            } => self.set_dnd(enabled, until_unix_ms).await,
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
            self.delete_where(
                "session_id=?1 AND closed_unix_ms IS NULL",
                vec![i64::from(notification.id).into()],
            )
            .await?;
            return Ok(());
        }
        ensure!(
            serde_json::to_vec(notification)?.len() <= PAGE_BYTES,
            "Notification exceeds history page byte limit"
        );
        let id = self.save_content(notification).await?;
        self.save_children(id, notification).await
    }
    async fn close_inner(&self, id: Option<u32>, closed: u64, reason: u32) -> Result<()> {
        self.connection.execute("UPDATE notifications SET closed_unix_ms=?1, close_reason=?2 WHERE closed_unix_ms IS NULL AND (?3 IS NULL OR session_id=?3)",
            params![integer(closed)?, i64::from(reason), id.map(i64::from)]).await?;
        Ok(())
    }
    pub(super) async fn delete(&mut self, positions: &[Position]) -> Result<()> {
        self.begin().await?;
        let result = async {
            // Bound SQL expression depth as well as parameter count; the
            // pinned engine's debug planner recursively visits OR terms.
            for chunk in positions.chunks(16) {
                let predicate =
                    std::iter::repeat_n("(session_id=? AND created_unix_ms=?)", chunk.len())
                        .collect::<Vec<_>>()
                        .join(" OR ");
                let mut values = Vec::with_capacity(chunk.len() * 2);
                for position in chunk {
                    values.push(i64::from(position.id).into());
                    values.push(integer(position.created)?.into());
                }
                self.delete_where(&predicate, values).await?;
            }
            Ok(())
        }
        .await;
        self.finish(result).await
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
        self.read_candidates(query).await
    }
    async fn record(&self, history_id: i64) -> Result<CatalogRecord> {
        Ok(catalog_record(self.records(&[history_id]).await?.remove(0)))
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
        let mut records = Vec::new();
        let mut bytes = 0;
        let ids = self.history_ids(query, before, limit, excluded).await?;
        // Bound full-content hydration as well as the final serialized page.
        // Large bodies can exhaust the byte budget well before 100 records.
        'pages: for chunk in ids.chunks(8) {
            for record in self.records(chunk).await? {
                let record = catalog_record(record);
                bytes += serde_json::to_vec(&record)?.len();
                records.push(record);
                // Include one overflow row so history::page can produce its cursor.
                if bytes > PAGE_BYTES {
                    break 'pages;
                }
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
        let mut rows = self.connection.query("SELECT history_id FROM notifications WHERE history_id < ?1 ORDER BY history_id DESC LIMIT ?2",
            params![before.unwrap_or(i64::MAX), i64::try_from(limit)?]).await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(row.get(0)?);
        }
        self.records(&ids).await
    }
}

fn catalog_record(record: HistoryNotification) -> CatalogRecord {
    CatalogRecord {
        history_id: Some(record.history_id),
        notification: record.notification,
        closed_unix_ms: record.closed_unix_ms,
        close_reason: record.close_reason,
    }
}
