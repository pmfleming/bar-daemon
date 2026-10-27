//! Service integration: all mutations (including todos.*) use one Turso owner.
use super::ActivityService;
use crate::activity::{
    agenda, agenda_adapter, agenda_projection::Projection, agenda_store::model::*, model::TodoItem,
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

impl ActivityService {
    pub(crate) async fn shutdown(&self) -> Result<()> {
        let _guard = self.todo_guard.lock().await;
        let store = self.data.read().await.agenda_store.clone();
        if let Some(store) = store {
            store.shutdown().await?;
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) async fn data_snapshot_for_test(&self) -> Snapshot {
        self.data
            .read()
            .await
            .agenda_store
            .as_ref()
            .unwrap()
            .snapshot()
            .await
            .unwrap()
    }
    pub(crate) async fn agenda_context(&self) -> Result<(Projection<chrono_tz::Tz>, String)> {
        self.agenda_context_at(Utc::now()).await
    }
    async fn agenda_context_at(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<(Projection<chrono_tz::Tz>, String)> {
        let name = self.state.read(|s| s.timezone.timezone.clone()).await;
        let system = || {
            std::env::var("TZ")
                .ok()
                .or_else(|| iana_time_zone::get_timezone().ok())
        };
        let zone: chrono_tz::Tz = name.parse().ok().or_else(|| system()?.trim_start_matches(':').parse().ok()).context("System timezone unavailable; agenda projection is fenced until a valid IANA zone is known")?;
        Ok((
            Projection {
                today: now.with_timezone(&zone).date_naive(),
                zone,
            },
            zone.name().into(),
        ))
    }
    pub(crate) async fn agenda_info(&self) -> Option<agenda::CatalogInfo> {
        self.agenda_info_at(Utc::now()).await
    }
    pub(super) async fn agenda_info_at(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Option<agenda::CatalogInfo> {
        let (p, timezone) = self.agenda_context_at(now).await.ok()?;
        let data = self.data.read().await;
        Some(agenda::CatalogInfo {
            catalog_revision: data.agenda_snapshot.as_ref()?.catalog_revision,
            import_revision: data.import_revision,
            today: p.today.to_string(),
            timezone,
        })
    }
    async fn cached_catalog(
        &self,
        p: &Projection<chrono_tz::Tz>,
        zone: &str,
    ) -> Result<(std::sync::Arc<agenda::Catalog>, u64, String)> {
        let mut data = self.data.write().await;
        ensure!(data.todo_error.is_none(), "Agenda store unavailable");
        let snapshot = data
            .agenda_snapshot
            .as_ref()
            .context("Agenda store unavailable")?;
        if let Some(c) = &data.agenda_catalog
            && c.revision == snapshot.catalog_revision
            && c.import_revision == data.import_revision
            && c.today == p.today
            && c.timezone == zone
        {
            return Ok((c.catalog.clone(), c.revision, c.imports.clone()));
        }
        let mut imports = data
            .events_by_source
            .values()
            .flatten()
            .map(agenda_adapter::imported)
            .collect::<Result<Vec<_>>>()?;
        imports.sort_by(|a, b| a.id.cmp(&b.id));
        for item in &mut imports {
            item.revision = snapshot.catalog_revision.max(1);
        }
        let digest = agenda::fingerprint(&imports);
        ensure!(
            imports.len() <= 10000,
            "imported agenda catalog exceeds 10000 records"
        );
        let mut items = snapshot.items.clone();
        items.extend(imports);
        let mut ids = std::collections::HashSet::new();
        ensure!(
            items.iter().all(|i| ids.insert(&i.id)),
            "agenda import identity collision"
        );
        let mut bytes = 0usize;
        for item in &items {
            bytes = bytes
                .checked_add(serde_json::to_vec(item)?.len())
                .context("agenda catalog size overflow")?;
            ensure!(
                bytes <= 100 * 1024 * 1024,
                "combined agenda catalog exceeds 100 MiB"
            );
        }
        let revision = snapshot.catalog_revision;
        let catalog = std::sync::Arc::new(agenda::Catalog::new(items, p));
        data.agenda_catalog = Some(agenda::CachedCatalog {
            revision,
            import_revision: data.import_revision,
            today: p.today,
            timezone: zone.into(),
            imports: digest.clone(),
            catalog: catalog.clone(),
        });
        Ok((catalog, revision, digest))
    }
    pub(super) async fn accept_calendar_imports(
        &self,
        imports: std::collections::HashMap<String, Vec<crate::activity::model::ActivityEvent>>,
    ) -> Result<()> {
        let _guard = self.todo_guard.lock().await;
        let data = self.data.read().await;
        if data.events_by_source == imports {
            return Ok(());
        }
        let snapshot = data
            .agenda_snapshot
            .as_ref()
            .context("Agenda store unavailable")?;
        let mut ids: std::collections::HashSet<_> =
            snapshot.items.iter().map(|i| i.id.clone()).collect();
        let mut bytes = 4096usize;
        for item in &snapshot.items {
            bytes = bytes.saturating_add(serde_json::to_vec(item)?.len());
        }
        let mut count = 0;
        for event in imports.values().flatten() {
            count += 1;
            let item = agenda_adapter::imported(event)?;
            ensure!(
                ids.insert(item.id.clone()),
                "agenda import identity collision"
            );
            bytes = bytes.saturating_add(serde_json::to_vec(&item)?.len());
            ensure!(
                count <= 10000 && bytes <= 100 * 1024 * 1024,
                "combined agenda import exceeds 10000 imported records or 100 MiB catalog budget"
            );
        }
        drop(data);
        let store = self.writable_agenda().await?;
        // Only actual source changes write a revision. Midnight/zone projection
        // changes never touch the durable store or stored endpoints.
        let revision = store.invalidate_catalog().await?;
        let mut data = self.data.write().await;
        data.agenda_snapshot
            .as_mut()
            .context("Agenda cache unavailable")?
            .catalog_revision = revision;
        data.events_by_source = imports;
        data.import_revision += 1;
        Ok(())
    }
    pub(crate) async fn agenda_list(
        &self,
        request: agenda::ListRequest,
    ) -> Result<agenda::ListResult> {
        let (p, zone) = self.agenda_context().await?;
        let (catalog, revision, imports) = self.cached_catalog(&p, &zone).await?;
        agenda::list(&catalog, revision, &imports, &p, &zone, request)
    }
    pub(crate) async fn agenda_markers(
        &self,
        request: agenda::MarkersRequest,
    ) -> Result<agenda::MarkersResult> {
        let (p, zone) = self.agenda_context().await?;
        let (catalog, revision, _) = self.cached_catalog(&p, &zone).await?;
        agenda::markers(&catalog, revision, &p, &zone, request)
    }
    pub(crate) async fn agenda_get(&self, id: &str) -> Result<Value> {
        ensure!(
            !id.trim().is_empty() && id.len() <= 512,
            "invalid agenda id"
        );
        let (p, zone) = self.agenda_context().await?;
        let (catalog, revision, _) = self.cached_catalog(&p, &zone).await?;
        let item = catalog.get(id).context("agenda item not found")?;
        Ok(json!({"item":item,"catalog_revision":revision}))
    }
    pub(crate) async fn agenda_receipt(&self, id: String) -> Result<Value> {
        ensure!(
            !id.trim().is_empty() && id.len() <= 512,
            "invalid request id"
        );
        let _guard = self.todo_guard.lock().await;
        let store = self
            .data
            .read()
            .await
            .agenda_store
            .clone()
            .context("Agenda store unavailable")?;
        let receipt = store.receipt(id).await?;
        let cached_revision = self
            .data
            .read()
            .await
            .agenda_snapshot
            .as_ref()
            .map_or(0, |s| s.catalog_revision);
        if receipt
            .as_ref()
            .is_some_and(|r| r.catalog_revision > cached_revision)
        {
            // Explicit recovery after an uncertain commit reconciles accepted
            // reads too. Never replay the write or alter its historical receipt.
            let snapshot = store.snapshot().await?;
            let mut data = self.data.write().await;
            data.todos = snapshot
                .items
                .iter()
                .filter_map(agenda_adapter::todo)
                .collect();
            data.agenda_snapshot = Some(snapshot);
            drop(data);
            self.publish_state(None).await;
        }
        Ok(json!({"receipt":receipt}))
    }
    fn legacy_request(&self) -> String {
        format!(
            "legacy-{}-{}",
            crate::time::unix_ms_i64(),
            self.todo_sequence.fetch_add(1, Ordering::Relaxed)
        )
    }
    async fn accept_agenda(&self, result: MutationResult) -> Result<MutationResult> {
        let (cached_revision, store) = {
            let data = self.data.read().await;
            (
                data.agenda_snapshot
                    .as_ref()
                    .map_or(0, |s| s.catalog_revision),
                data.agenda_store.clone(),
            )
        };
        if result.catalog_revision > cached_revision.saturating_add(1) {
            // A missing completion must not leave an incremental cache gap.
            let recovered = store
                .context("Agenda store unavailable")?
                .snapshot()
                .await?;
            let mut data = self.data.write().await;
            data.todos = recovered
                .items
                .iter()
                .filter_map(agenda_adapter::todo)
                .collect();
            data.agenda_snapshot = Some(recovered);
            drop(data);
            self.publish_state(None).await;
            return Ok(result);
        }
        let changed = {
            let mut data = self.data.write().await;
            let snapshot = data
                .agenda_snapshot
                .as_mut()
                .context("Agenda cache unavailable; resolve request by receipt")?;
            // An explicit duplicate request may return an older receipt. Never
            // roll the accepted cache back to that old record revision.
            let changed = result.catalog_revision > snapshot.catalog_revision;
            if changed {
                if let Some(item) = &result.item {
                    snapshot.items.retain(|r| r.id != item.id);
                    snapshot.items.push(item.clone());
                }
                if let Some(id) = &result.deleted_id {
                    snapshot.items.retain(|r| &r.id != id);
                }
                snapshot.catalog_revision = result.catalog_revision;
            }
            data.todos = snapshot
                .items
                .iter()
                .filter_map(agenda_adapter::todo)
                .collect();
            changed
        };
        if changed {
            self.publish_state(None).await;
        }
        Ok(result)
    }
    async fn check_agenda_capacity(&self, replacing: Option<&str>, extra: usize) -> Result<()> {
        let data = self.data.read().await;
        let snapshot = data
            .agenda_snapshot
            .as_ref()
            .context("Agenda store unavailable")?;
        let mut bytes = extra.saturating_add(4096); // reserved record metadata growth
        for item in snapshot
            .items
            .iter()
            .filter(|i| Some(i.id.as_str()) != replacing)
        {
            bytes = bytes.saturating_add(serde_json::to_vec(item)?.len());
        }
        for event in data.events_by_source.values().flatten() {
            bytes =
                bytes.saturating_add(serde_json::to_vec(&agenda_adapter::imported(event)?)?.len());
        }
        ensure!(
            bytes <= 100 * 1024 * 1024,
            "combined agenda catalog exceeds 100 MiB; remove records or sources before retrying"
        );
        Ok(())
    }
    pub(crate) async fn agenda_create(
        &self,
        request_id: String,
        item: NewItem,
    ) -> Result<MutationResult> {
        let _guard = self.todo_guard.lock().await;
        let store = self.writable_agenda().await?;
        if store.receipt(request_id.clone()).await?.is_none() {
            self.check_agenda_capacity(None, serde_json::to_vec(&item)?.len())
                .await?;
        }
        let result = store.create(request_id, item).await?;
        self.accept_agenda(result).await
    }
    pub(crate) async fn agenda_patch(
        &self,
        request_id: String,
        id: String,
        revision: u64,
        patch: ItemPatch,
    ) -> Result<MutationResult> {
        let _guard = self.todo_guard.lock().await;
        let store = self.writable_agenda().await?;
        if store.receipt(request_id.clone()).await?.is_none() {
            let mut candidate = store
                .get(id.clone())
                .await?
                .context("agenda item not found or imported calendar is read-only")?;
            if let Some(v) = &patch.title {
                candidate.title = v.clone();
            }
            if let Some(v) = &patch.notes {
                candidate.notes = v.clone();
            }
            if let Some(v) = &patch.links {
                candidate.links = v.clone();
            }
            if let Some(v) = &patch.schedule {
                candidate.schedule = v.clone();
            }
            if let Some(v) = &patch.event {
                candidate.event = Some(v.clone());
            }
            if let Some(v) = &patch.task {
                candidate.task = Some(v.clone());
            }
            self.check_agenda_capacity(Some(&id), serde_json::to_vec(&candidate)?.len())
                .await?;
        }
        let result = store.patch(request_id, id, revision, patch).await?;
        self.accept_agenda(result).await
    }
    pub(crate) async fn agenda_delete(
        &self,
        request_id: String,
        id: String,
        revision: u64,
    ) -> Result<MutationResult> {
        let _guard = self.todo_guard.lock().await;
        let store = self.writable_agenda().await?;
        let result = store.delete(request_id, id, revision).await?;
        self.accept_agenda(result).await
    }
    pub(crate) async fn create_todo(
        &self,
        title: String,
        due_unix_ms: Option<i64>,
        due_date: Option<String>,
        priority: u8,
    ) -> Result<TodoItem> {
        ensure!(!title.trim().is_empty(), "todo title cannot be empty");
        ensure!(priority <= 9, "todo priority must be between 0 and 9");
        ensure!(
            due_date.is_none() || due_unix_ms.is_none(),
            "todo due date and instant are incompatible"
        );
        let schedule = if due_unix_ms.is_some() {
            let (_, timezone) = self.agenda_context().await?;
            Schedule {
                mode: ScheduleMode::Timed,
                end_unix_ms: due_unix_ms,
                timezone: Some(timezone),
                ..Schedule::default()
            }
        } else {
            Schedule {
                end_date: due_date,
                ..Schedule::default()
            }
        };
        let result = self
            .agenda_create(
                self.legacy_request(),
                NewItem {
                    kind: ItemKind::Task,
                    title: title.trim().into(),
                    notes: String::new(),
                    links: vec![],
                    schedule,
                    task: Some(TaskDetails {
                        priority: priority.into(),
                        ..TaskDetails::default()
                    }),
                    event: None,
                },
            )
            .await?;
        agenda_adapter::todo(result.item.as_ref().context("missing created task")?)
            .context("missing task details")
    }
    pub(crate) async fn complete_todo(&self, id: &str, completed: bool) -> Result<TodoItem> {
        let _guard = self.todo_guard.lock().await;
        let store = self.writable_agenda().await?;
        let record = store
            .get(id.into())
            .await?
            .with_context(|| format!("todo {id} was not found"))?;
        let mut task = record.task.context("item is not a task")?;
        task.completed = completed;
        task.completed_unix_ms = completed.then(crate::time::unix_ms_i64);
        let result = store
            .patch(
                self.legacy_request(),
                id.into(),
                record.revision,
                ItemPatch {
                    task: Some(task),
                    ..ItemPatch::default()
                },
            )
            .await?;
        let result = self.accept_agenda(result).await?;
        agenda_adapter::todo(result.item.as_ref().context("missing task")?)
            .context("missing task details")
    }
    pub(crate) async fn delete_todo(&self, id: &str) -> Result<()> {
        let _guard = self.todo_guard.lock().await;
        let store = self.writable_agenda().await?;
        let record = store
            .get(id.into())
            .await?
            .with_context(|| format!("todo {id} was not found"))?;
        ensure!(record.kind == ItemKind::Task, "item is not a task");
        let result = store
            .delete(self.legacy_request(), id.into(), record.revision)
            .await?;
        self.accept_agenda(result).await?;
        Ok(())
    }
}
