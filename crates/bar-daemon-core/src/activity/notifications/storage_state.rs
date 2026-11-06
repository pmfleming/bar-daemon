//! Typed notification preferences and durable identity state.
use super::rows::{boolean, optional_integer};
use super::*;

impl NotificationStore {
    pub(in super::super) async fn last_id(&self) -> Result<u32> {
        self.healthy()?;
        Ok(u32::try_from(
            self.integer("SELECT last_notification_id FROM notification_state WHERE singleton=1")
                .await?,
        )?)
    }
    pub(super) async fn remember_id(&self, id: u32) -> Result<()> {
        self.connection.execute("UPDATE notification_state SET last_notification_id=max(last_notification_id,?1) WHERE singleton=1", [i64::from(id)]).await?;
        Ok(())
    }
    pub(in super::super) async fn load_dnd(&self) -> Result<(bool, Option<u64>)> {
        self.healthy()?;
        let row = self
            .connection
            .query(
                "SELECT dnd,dnd_until_unix_ms FROM notification_state WHERE singleton=1",
                (),
            )
            .await?
            .next()
            .await?
            .context("notification state missing")?;
        Ok((
            boolean(&row, 0)?,
            row.get::<Option<i64>>(1)?.map(u64::try_from).transpose()?,
        ))
    }
    pub(super) async fn set_dnd(&self, enabled: bool, until: Option<u64>) -> Result<()> {
        self.connection
            .execute(
                "UPDATE notification_state SET dnd=?1,dnd_until_unix_ms=?2 WHERE singleton=1",
                params![
                    enabled,
                    optional_integer(enabled.then_some(until).flatten())?
                ],
            )
            .await?;
        Ok(())
    }
    pub(super) async fn load_settings(&self) -> Result<Settings> {
        let row = self.connection.query("SELECT countdown,code_expiry_seconds,history_days FROM notification_settings WHERE singleton=1", ()).await?.next().await?.context("notification settings missing")?;
        let settings = Settings {
            countdown: boolean(&row, 0)?,
            code_expiry_seconds: u16::try_from(row.get::<i64>(1)?)?,
            history_days: u8::try_from(row.get::<i64>(2)?)?,
        };
        settings.validate()?;
        Ok(settings)
    }
    pub(in super::super) async fn settings(&mut self, value: Option<Settings>) -> Result<Settings> {
        self.healthy()?;
        let Some(value) = value else {
            return self.load_settings().await;
        };
        value.validate()?;
        self.begin().await?;
        let result = async {
            self.connection.execute("UPDATE notification_settings SET countdown=?1,code_expiry_seconds=?2,history_days=?3 WHERE singleton=1",
                params![value.countdown, i64::from(value.code_expiry_seconds), i64::from(value.history_days)]).await?;
            self.prune_inner().await?;
            Ok(value)
        }.await;
        self.finish(result).await
    }
    pub(in super::super) async fn set_policy(
        &mut self,
        key: &str,
        policy: &AppPolicy,
    ) -> Result<()> {
        // Check storage conversions before BEGIN: conversion failure must not
        // strand an open transaction. Runtime expiry validation belongs to Rust.
        let until = optional_integer(policy.until_unix_ms)?;
        self.begin().await?;
        let result = self.connection.execute(
            "INSERT INTO notification_app_policies VALUES (?1,?2,?3,?4,?5) ON CONFLICT(app_key) DO UPDATE SET silent=excluded.silent,until_unix_ms=excluded.until_unix_ms,group_similar=excluded.group_similar,bypass_dnd=excluded.bypass_dnd",
            params![key, policy.silent, until, policy.group_similar, policy.bypass_dnd]).await.map(|_| ()).map_err(anyhow::Error::new);
        self.finish(result).await
    }
    pub(in super::super) async fn policies(&self) -> Result<BTreeMap<String, AppPolicy>> {
        self.healthy()?;
        let mut rows = self.connection.query("SELECT app_key,silent,until_unix_ms,group_similar,bypass_dnd FROM notification_app_policies", ()).await?;
        let mut policies = BTreeMap::new();
        while let Some(row) = rows.next().await? {
            policies.insert(
                row.get(0)?,
                AppPolicy {
                    silent: boolean(&row, 1)?,
                    until_unix_ms: row.get::<Option<i64>>(2)?.map(u64::try_from).transpose()?,
                    group_similar: boolean(&row, 3)?,
                    bypass_dnd: boolean(&row, 4)?,
                },
            );
        }
        Ok(policies)
    }
}
