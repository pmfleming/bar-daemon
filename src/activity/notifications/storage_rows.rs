//! Typed content/preview encoding. JSON is only used to enforce the wire budget.
use super::super::{
    context_actions::ContextAction,
    model::{NotificationAction, NotificationHints},
};
use super::*;

const CONTENT_COLUMNS: &str = "session_id,app_key,app_name,app_icon,identity_icon,summary,body,urgency,category,desktop_entry,image_path,sound_name,sound_file,resident,suppress_sound,image_data_present,created_unix_ms,updated_unix_ms,expires_unix_ms,group_key,source_monitor,snoozed_until_unix_ms,dnd_bypass,search_text";

pub(super) fn optional_integer(value: Option<u64>) -> Result<Option<i64>> {
    value.map(integer).transpose()
}
fn unsigned(row: &turso::Row, index: usize) -> Result<u64> {
    Ok(u64::try_from(row.get::<i64>(index)?)?)
}
fn optional_unsigned(row: &turso::Row, index: usize) -> Result<Option<u64>> {
    Ok(row
        .get::<Option<i64>>(index)?
        .map(u64::try_from)
        .transpose()?)
}
pub(super) fn boolean(row: &turso::Row, index: usize) -> Result<bool> {
    match row.get::<i64>(index)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => anyhow::bail!("invalid notification boolean"),
    }
}
pub(super) fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

impl NotificationStore {
    pub(super) async fn save_content(&self, n: &ActiveNotification) -> Result<i64> {
        let existing = self.connection.query(
            "SELECT history_id FROM notifications WHERE session_id=?1 AND closed_unix_ms IS NULL",
            [i64::from(n.id)],
        ).await?.next().await?.map(|row| row.get::<i64>(0)).transpose()?;
        let values = params![
            existing,
            i64::from(n.id),
            n.app_key(),
            n.app_name.clone(),
            n.app_icon.clone(),
            n.identity_icon.clone(),
            n.summary.clone(),
            n.body.clone(),
            i64::from(n.hints.urgency),
            n.hints.category.clone(),
            n.hints.desktop_entry.clone(),
            n.hints.image_path.clone(),
            n.hints.sound_name.clone(),
            n.hints.sound_file.clone(),
            n.hints.resident,
            n.hints.suppress_sound,
            n.hints.image_data_present,
            integer(n.created_unix_ms)?,
            integer(n.updated_unix_ms)?,
            optional_integer(n.expires_unix_ms)?,
            n.group_key.clone(),
            n.source_monitor.clone(),
            optional_integer(n.snoozed_until_unix_ms)?,
            n.dnd_bypass,
            search_text(n)
        ];
        let updates = CONTENT_COLUMNS
            .split(',')
            .map(|column| format!("{column}=excluded.{column}"))
            .collect::<Vec<_>>()
            .join(",");
        self.connection.execute(format!(
            "INSERT INTO notifications(history_id,{CONTENT_COLUMNS}) VALUES ({}) ON CONFLICT(history_id) DO UPDATE SET {updates}",
            placeholders(values.len())), values).await?;
        let id = self.connection.query(
            "SELECT history_id FROM notifications WHERE session_id=?1 AND closed_unix_ms IS NULL",
            [i64::from(n.id)],
        ).await?.next().await?.context("saved notification missing")?.get(0)?;
        Ok(id)
    }

    pub(super) async fn save_children(&self, id: i64, n: &ActiveNotification) -> Result<()> {
        for table in [
            "notification_actions",
            "notification_context_actions",
            "notification_previews",
        ] {
            self.connection
                .execute(format!("DELETE FROM {table} WHERE history_id=?1"), [id])
                .await?;
        }
        if !n.actions.is_empty() {
            let mut statement = self
                .connection
                .prepare("INSERT INTO notification_actions VALUES (?1,?2,?3,?4)")
                .await?;
            for (ordinal, action) in n.actions.iter().enumerate() {
                statement
                    .execute(params![
                        id,
                        i64::try_from(ordinal)?,
                        action.key.clone(),
                        action.label.clone()
                    ])
                    .await?;
            }
        }
        if !n.context_actions.is_empty() {
            let mut statement = self
                .connection
                .prepare("INSERT INTO notification_context_actions VALUES (?1,?2,?3,?4,?5)")
                .await?;
            for (ordinal, action) in n.context_actions.iter().enumerate() {
                statement
                    .execute(params![
                        id,
                        i64::try_from(ordinal)?,
                        action.key.clone(),
                        action.label.clone(),
                        action.icon.clone()
                    ])
                    .await?;
            }
        }
        let p = Preview::from_active(n, "");
        self.connection
            .execute(
                "INSERT INTO notification_previews VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    id,
                    p.app_name,
                    p.app_icon,
                    p.identity_icon,
                    p.hints.desktop_entry,
                    p.hints.image_path,
                    p.summary,
                    p.body,
                    p.group_key,
                    p.repeat_key.as_ref().map(|key| key.2.clone()),
                    p.repeat_key.as_ref().map(|key| i64::from(key.3))
                ],
            )
            .await?;
        Ok(())
    }

    // The parent predicate is evaluated before removing parents, in the same
    // transaction, so prune, explicit delete and transient replacement cannot
    // leave orphan actions/previews even when a later statement fails.
    pub(super) async fn delete_where(
        &self,
        predicate: &str,
        values: Vec<turso::Value>,
    ) -> Result<usize> {
        for table in [
            "notification_actions",
            "notification_context_actions",
            "notification_previews",
        ] {
            self.connection.execute(format!("DELETE FROM {table} WHERE history_id IN (SELECT history_id FROM notifications WHERE {predicate})"), values.clone()).await?;
        }
        Ok(usize::try_from(
            self.connection
                .execute(
                    format!("DELETE FROM notifications WHERE {predicate}"),
                    values,
                )
                .await?,
        )?)
    }

    pub(super) async fn read_candidates(&self, query: &str) -> Result<Vec<Preview>> {
        self.healthy()?;
        // Select the scope BEFORE joining/filtering. Search does not reach older
        // rows, and empty search never transfers normalized full bodies.
        let search_column = if query.is_empty() {
            "''"
        } else {
            "n.search_text"
        };
        let mut rows = self.connection.query(format!(
            "SELECT n.history_id,n.session_id,n.created_unix_ms,n.app_key,p.app_name,p.app_icon,p.identity_icon,p.desktop_entry,p.image_path,p.summary,p.body,n.closed_unix_ms,n.snoozed_until_unix_ms,p.group_key,p.repeat_category,p.repeat_urgency,{search_column},p.history_id
             FROM (SELECT history_id FROM notifications ORDER BY history_id DESC LIMIT ?1) scope
             JOIN notifications n ON n.history_id=scope.history_id
             LEFT JOIN notification_previews p ON p.history_id=n.history_id
             ORDER BY n.history_id DESC"), [i64::try_from(SCOPE_LIMIT)?]).await?;
        let mut previews = Vec::new();
        while let Some(row) = rows.next().await? {
            ensure!(
                row.get::<Option<i64>>(17)?.is_some(),
                "notification preview missing"
            );
            let summary: String = row.get(9)?;
            let body: String = row.get(10)?;
            let category: Option<String> = row.get(14)?;
            let urgency: Option<i64> = row.get(15)?;
            ensure!(
                category.is_some() == urgency.is_some(),
                "invalid repeat projection"
            );
            let repeat_key = category
                .zip(urgency)
                .map(|(category, urgency)| {
                    Ok::<_, anyhow::Error>((
                        summary.clone(),
                        body.clone(),
                        category,
                        u8::try_from(urgency)?,
                    ))
                })
                .transpose()?;
            previews.push(Preview {
                history_id: Some(row.get(0)?),
                id: u32::try_from(row.get::<i64>(1)?)?,
                created_unix_ms: unsigned(&row, 2)?,
                app_key: row.get(3)?,
                app_name: row.get(4)?,
                app_icon: row.get(5)?,
                identity_icon: row.get(6)?,
                hints: center::PreviewHints {
                    desktop_entry: row.get(7)?,
                    image_path: row.get(8)?,
                },
                summary,
                body,
                closed_unix_ms: optional_unsigned(&row, 11)?,
                snoozed_until_unix_ms: optional_unsigned(&row, 12)?,
                group_key: row.get(13)?,
                repeat_key,
                matches: query.is_empty() || row.get::<String>(16)?.contains(query),
            });
        }
        Ok(previews)
    }

    // Three queries per bounded chunk, independent of action count. Reorder
    // after hydration to retain the caller's history/cursor order.
    pub(super) async fn records(&self, ids: &[i64]) -> Result<Vec<HistoryNotification>> {
        self.healthy()?;
        let mut result = Vec::new();
        for ids in ids.chunks(MAX_PAGE + 1) {
            let slots = placeholders(ids.len());
            let mut rows = self.connection.query(format!("SELECT history_id,{CONTENT_COLUMNS},closed_unix_ms,close_reason FROM notifications WHERE history_id IN ({slots})"), ids.to_vec()).await?;
            let mut records = BTreeMap::new();
            while let Some(row) = rows.next().await? {
                let record = decode_record(&row)?;
                records.insert(record.history_id, record);
            }
            let mut rows = self.connection.query(format!("SELECT history_id,action_key,label FROM notification_actions WHERE history_id IN ({slots}) ORDER BY history_id,ordinal"), ids.to_vec()).await?;
            while let Some(row) = rows.next().await? {
                let id = row.get::<i64>(0)?;
                let record = records.get_mut(&id).context("orphan notification action")?;
                record.notification.actions.push(NotificationAction {
                    key: row.get(1)?,
                    label: row.get(2)?,
                });
            }
            let mut rows = self.connection.query(format!("SELECT history_id,action_key,label,icon FROM notification_context_actions WHERE history_id IN ({slots}) ORDER BY history_id,ordinal"), ids.to_vec()).await?;
            while let Some(row) = rows.next().await? {
                let id = row.get::<i64>(0)?;
                let record = records
                    .get_mut(&id)
                    .context("orphan notification context action")?;
                record.notification.context_actions.push(ContextAction {
                    key: row.get(1)?,
                    label: row.get(2)?,
                    icon: row.get(3)?,
                });
            }
            for id in ids {
                let record = records
                    .remove(id)
                    .context("notification no longer retained")?;
                ensure!(
                    serde_json::to_vec(&record.notification)?.len() <= PAGE_BYTES,
                    "Notification exceeds history page byte limit"
                );
                result.push(record);
            }
        }
        Ok(result)
    }
}

fn decode_record(row: &turso::Row) -> Result<HistoryNotification> {
    Ok(HistoryNotification {
        history_id: row.get(0)?,
        notification: ActiveNotification {
            id: u32::try_from(row.get::<i64>(1)?)?,
            app_name: row.get(3)?,
            app_icon: row.get(4)?,
            identity_icon: row.get(5)?,
            summary: row.get(6)?,
            body: row.get(7)?,
            hints: NotificationHints {
                urgency: u8::try_from(row.get::<i64>(8)?)?,
                category: row.get(9)?,
                desktop_entry: row.get(10)?,
                image_path: row.get(11)?,
                sound_name: row.get(12)?,
                sound_file: row.get(13)?,
                resident: boolean(row, 14)?,
                suppress_sound: boolean(row, 15)?,
                image_data_present: boolean(row, 16)?,
                transient: false,
            },
            created_unix_ms: unsigned(row, 17)?,
            updated_unix_ms: unsigned(row, 18)?,
            expires_unix_ms: optional_unsigned(row, 19)?,
            group_key: row.get(20)?,
            source_monitor: row.get(21)?,
            snoozed_until_unix_ms: optional_unsigned(row, 22)?,
            dnd_bypass: boolean(row, 23)?,
            actions: Vec::new(),
            context_actions: Vec::new(),
            interaction_blocked: false,
            toast_visible: false,
            toast_expires_unix_ms: None,
        },
        closed_unix_ms: optional_unsigned(row, 25)?,
        close_reason: row.get::<Option<i64>>(26)?.map(u32::try_from).transpose()?,
    })
}
