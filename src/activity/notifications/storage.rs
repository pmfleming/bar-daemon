//! Notification operations; futures are driven only by the dedicated owner thread.
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
        let changed = self.connection.execute(
            "UPDATE notifications SET payload_json=?2, updated_unix_ms=?3, search_text=?4 WHERE session_id=?1 AND closed_unix_ms IS NULL",
            params![i64::from(notification.id), payload.clone(), integer(notification.updated_unix_ms)?, search.clone()]).await?;
        if changed == 0 {
            self.connection.execute("INSERT INTO notifications(session_id,payload_json,created_unix_ms,updated_unix_ms,search_text) VALUES (?1,?2,?3,?4,?5)",
                params![i64::from(notification.id), payload, integer(notification.created_unix_ms)?, integer(notification.updated_unix_ms)?, search]).await?;
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
            for position in positions {
                self.connection
                    .execute(
                        "DELETE FROM notifications WHERE session_id=?1 AND created_unix_ms=?2",
                        params![i64::from(position.id), integer(position.created)?],
                    )
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
        let mut rows = self
            .connection
            .query(
                "SELECT history_id,payload_json,closed_unix_ms,close_reason FROM notifications",
                (),
            )
            .await?;
        let mut targets = Vec::new();
        while let Some(row) = rows.next().await? {
            let record = history_record(&row)?;
            let n = &record.notification;
            if app_key.is_none_or(|key| key == n.app_key())
                && selected.is_none_or(|p| p.id == n.id && p.created == n.created_unix_ms)
            {
                ensure!(
                    targets.len() < 100_000,
                    "Deletion exceeds 100,000-record safety limit"
                );
                targets.push(Position {
                    id: n.id,
                    created: n.created_unix_ms,
                });
            }
        }
        Ok(targets)
    }
    async fn candidates(&self) -> Result<Vec<CatalogRecord>> {
        self.healthy()?;
        let mut rows = self.connection.query("SELECT history_id,payload_json,closed_unix_ms,close_reason FROM notifications ORDER BY history_id DESC LIMIT ?1", [i64::try_from(SCOPE_LIMIT)?]).await?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().await? {
            records.push(catalog_record(&row)?);
        }
        Ok(records)
    }
    pub(super) async fn center(
        &self,
        query: &CenterQuery,
        active: &[ActiveNotification],
        epoch: &str,
        revision: u64,
    ) -> Result<CenterPage> {
        let records = self.candidates().await?;
        let mut previews: Vec<_> = records
            .iter()
            .filter(|r| {
                !active.iter().any(|n| {
                    n.id == r.notification.id && n.created_unix_ms == r.notification.created_unix_ms
                })
            })
            .map(|r| {
                let mut p = Preview::from_active(&r.notification, &query.query);
                p.history_id = r.history_id;
                p.closed_unix_ms = r.closed_unix_ms;
                p
            })
            .collect();
        previews.extend(active.iter().map(|n| Preview::from_active(n, &query.query)));
        center::project(previews, query, epoch, revision, |preview| {
            if let Some(n) = active
                .iter()
                .find(|n| n.id == preview.id && n.created_unix_ms == preview.created_unix_ms)
            {
                return Ok(n.clone().into());
            }
            records
                .iter()
                .find(|r| r.history_id == preview.history_id)
                .cloned()
                .context("notification no longer retained")
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
        let mut records = self.candidates().await?;
        records.retain(|r| {
            let n = &r.notification;
            before.is_none_or(|p| p.contains(n))
                && !excluded.contains(&(n.created_unix_ms, n.id))
                && search_text(n).contains(query)
        });
        records.sort_by_key(|r| {
            std::cmp::Reverse((r.notification.created_unix_ms, r.notification.id))
        });
        records.truncate(limit + 1);
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
