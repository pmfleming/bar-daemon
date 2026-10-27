//! Record-level mutation policy, independent of transaction and SQL mechanics.
use super::model::*;
use anyhow::{Context, Result, anyhow, ensure};

pub(super) fn create_record(id: String, item: NewItem, now: i64) -> Record {
    let mut record = Record {
        id,
        kind: item.kind,
        source_id: "local".into(),
        source_uid: None,
        revision: 1,
        title: item.title,
        notes: item.notes,
        links: item.links,
        schedule: item.schedule,
        created_unix_ms: now,
        modified_unix_ms: now,
        capabilities: Capabilities {
            complete: item.kind == ItemKind::Task,
            ..Capabilities::local()
        },
        event: item.event,
        task: item.task,
    };
    match record.kind {
        ItemKind::Event => {
            record.event.get_or_insert_with(Default::default);
        }
        ItemKind::Task => {
            record.task.get_or_insert_with(Default::default);
        }
    }
    if let Some(task) = &mut record.task
        && task.completed
        && task.completed_unix_ms.is_none()
    {
        task.completed_unix_ms = Some(now);
    }
    record
}

pub(super) fn patch_record(
    mut record: Record,
    expected_revision: u64,
    patch: ItemPatch,
    now: i64,
) -> Result<Record> {
    ensure!(record.revision == expected_revision, "revision conflict");
    ensure!(record.capabilities.write, "source is read-only");
    if let Some(mut task) = patch.task {
        ensure!(record.kind == ItemKind::Task, "task fields on event");
        // Stored reads already validate this invariant, but mutation policy also
        // makes its requirement explicit rather than panicking on a missing task.
        let old = record.task.as_ref().context("task details required")?;
        if task.completed != old.completed {
            ensure!(record.capabilities.complete, "source cannot complete");
        }
        if task.completed && task.completed_unix_ms.is_none() {
            task.completed_unix_ms = if old.completed {
                old.completed_unix_ms
            } else {
                Some(now)
            };
        }
        if !task.completed {
            task.completed_unix_ms = None;
        }
        record.task = Some(task);
    }
    if let Some(value) = patch.title {
        record.title = value;
    }
    if let Some(value) = patch.notes {
        record.notes = value;
    }
    if let Some(value) = patch.links {
        record.links = value;
    }
    if let Some(value) = patch.schedule {
        record.schedule = value;
    }
    if let Some(value) = patch.event {
        record.event = Some(value);
    }
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or_else(|| anyhow!("revision exhausted"))?;
    record.modified_unix_ms = now;
    record.validate()?;
    Ok(record)
}
