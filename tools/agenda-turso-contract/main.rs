//! Isolated engine smoke contract; never opens a user-selected store.
//! Never defaults to an application's live state directory.
#[cfg(not(test))]
#[path = "../../src/activity/agenda_store/mod.rs"]
mod agenda_store;

#[cfg(not(test))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use agenda_store::{
        Store,
        model::{ItemKind, ItemPatch, NewItem},
    };
    use anyhow::Context;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(
        args.is_empty(),
        "usage: agenda-turso-contract (isolated temporary store only)"
    );
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("agenda");
    let store = Store::open(root.clone(), None).await?;
    let item = NewItem {
        kind: ItemKind::Event,
        title: "isolated engine contract".into(),
        notes: "x".repeat(262144),
        links: vec![],
        schedule: Default::default(),
        event: None,
        task: None,
    };
    let result = store.create("engine-contract".into(), item).await?;
    let created = result.item.as_ref().context("missing committed event")?;
    anyhow::ensure!(!created.capabilities.complete, "event cannot complete");
    let changed = store
        .patch(
            "engine-patch".into(),
            created.id.clone(),
            created.revision,
            ItemPatch {
                title: Some("edited engine contract".into()),
                ..Default::default()
            },
        )
        .await?;
    let accepted = changed.item.as_ref().context("missing patched event")?;
    anyhow::ensure!(
        store.get(accepted.id.clone()).await?.as_ref() == Some(accepted),
        "get/patch mismatch"
    );
    let catalog_revision = store.invalidate_catalog().await?;
    let export = dir.path().join("export.json");
    store.export(export.clone()).await?;
    store.shutdown().await?;

    let reader = Store::open_existing(root.clone()).await?;
    anyhow::ensure!(
        reader.snapshot().await?.catalog_revision == catalog_revision,
        "catalog revision lost on restart"
    );
    anyhow::ensure!(
        reader.receipt("engine-contract".into()).await? == Some(result.clone()),
        "historical receipt changed after patch/restart"
    );
    reader.shutdown().await?;

    let store = Store::open(root, None).await?;
    anyhow::ensure!(
        store.receipt("engine-patch".into()).await? == Some(changed.clone()),
        "patch receipt lost on restart"
    );
    let restored = Store::open(dir.path().join("restored"), None).await?;
    restored.restore(export).await?;
    anyhow::ensure!(
        restored.get(accepted.id.clone()).await?.as_ref() == Some(accepted),
        "restored record mismatch"
    );
    let deletion = store
        .delete(
            "engine-delete".into(),
            accepted.id.clone(),
            accepted.revision,
        )
        .await?;
    anyhow::ensure!(
        deletion.deleted_id.as_ref() == Some(&accepted.id)
            && store.get(accepted.id.clone()).await?.is_none(),
        "delete was not accepted"
    );
    anyhow::ensure!(
        store.receipt("engine-delete".into()).await? == Some(deletion),
        "delete receipt missing"
    );
    anyhow::ensure!(
        restored.snapshot().await?.items == vec![accepted.clone()],
        "delete affected restored owner"
    );
    store.shutdown().await?;
    restored.shutdown().await?;
    println!(
        "PASS: Turso bindings 0.8.0 / Syscall I/O / FULL CRUD, large bounded payload, readonly restart, durable catalog/receipts, private logical export and restore"
    );
    Ok(())
}
#[cfg(test)]
fn main() {}
