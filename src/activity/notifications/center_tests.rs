use super::{
    center::{CenterPage, CenterQuery},
    engine::NotificationEngine,
    history::{HistoryError, Position},
    model::{IncomingNotification, NotificationHints},
};
use crate::state::StateStore;

fn query(view: &str) -> CenterQuery {
    serde_json::from_value(serde_json::json!({"view": view})).unwrap()
}
fn notification(app: &str, summary: &str) -> IncomingNotification {
    IncomingNotification {
        app_name: app.into(),
        app_icon: String::new(),
        summary: summary.into(),
        body: "A body searchable beyond the first page".into(),
        actions: vec![],
        hints: NotificationHints {
            desktop_entry: app.into(),
            ..Default::default()
        },
        expire_timeout: 0,
    }
}
#[tokio::test]
async fn center_groups_full_scope_and_seeks_without_loading_intermediate_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let engine =
        NotificationEngine::persistent(StateStore::default(), dir.path().join("history.db"))
            .await
            .unwrap();
    let mut oldest = 0;
    for index in 0..500 {
        let id = engine
            .notify(0, notification("chat", &format!("Message {index}")))
            .await
            .unwrap();
        if index == 0 {
            oldest = id;
        }
        engine.dismiss(id).await.unwrap();
    }
    engine
        .notify(0, notification("mail", "Inbox"))
        .await
        .unwrap();
    let CenterPage::Apps {
        apps, total_apps, ..
    } = engine.query_center(query("apps")).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(total_apps, 2);
    assert_eq!(apps[1].count, 500);
    assert_eq!(apps[1].total_count, 500);
    let mut request = query("app");
    request.app_key = Some("desktop:chat".into());
    request.page = 100;
    let CenterPage::App {
        count,
        pages,
        page,
        entries,
        overview,
        ..
    } = engine.query_center(request.clone()).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(
        (count, pages, page, entries.len(), overview.len()),
        (500, 100, 100, 5, 3)
    );
    let last = entries.last().unwrap();
    assert_eq!(last.id, oldest);
    request.selected = Some(Position {
        created: last.created_unix_ms,
        id: oldest,
    });
    let CenterPage::App { selected, .. } = engine.query_center(request.clone()).await.unwrap()
    else {
        panic!()
    };
    let selected = selected.unwrap();
    assert_eq!(selected.notification.id, oldest);
    assert_eq!(
        selected.notification.body,
        "A body searchable beyond the first page"
    );
    assert!(selected.closed_unix_ms.is_some());
    request.query = "Message 0".into();
    let CenterPage::App {
        count,
        total_count,
        page,
        entries,
        selected,
        ..
    } = engine.query_center(request).await.unwrap()
    else {
        panic!()
    };
    assert_eq!((count, total_count, page, entries.len()), (1, 500, 1, 1));
    assert_eq!(selected.unwrap().notification.id, oldest);
}
#[tokio::test]
async fn center_revision_fences_app_pages_and_retains_selected_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let engine = NotificationEngine::persistent(StateStore::default(), path.clone())
        .await
        .unwrap();
    for index in 0..65 {
        engine
            .notify(0, notification(&format!("app{index}"), "hello"))
            .await
            .unwrap();
    }
    let CenterPage::Apps {
        epoch,
        revision,
        next_offset,
        apps,
        ..
    } = engine.query_center(query("apps")).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(apps.len(), 50);
    let mut next = query("apps");
    next.offset = next_offset.unwrap();
    next.epoch = Some(epoch);
    next.revision = Some(revision);
    let CenterPage::Apps {
        apps, next_offset, ..
    } = engine.query_center(next.clone()).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(apps.len(), 15);
    assert!(next_offset.is_none());
    engine.set_dnd(true, None).await.unwrap();
    assert!(engine.query_center(next.clone()).await.is_ok());
    engine
        .notify(0, notification("another", "arrival"))
        .await
        .unwrap();
    assert!(matches!(
        engine.query_center(next.clone()).await,
        Err(HistoryError::Stale)
    ));
    let restarted = NotificationEngine::persistent(StateStore::default(), path)
        .await
        .unwrap();
    assert!(matches!(
        restarted.query_center(next).await,
        Err(HistoryError::Stale)
    ));
}
#[tokio::test]
async fn center_deduplicates_live_and_never_resurrects_transient_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let engine =
        NotificationEngine::persistent(StateStore::default(), dir.path().join("history.db"))
            .await
            .unwrap();
    let id = engine
        .notify(0, notification("chat", "original"))
        .await
        .unwrap();
    let CenterPage::Apps { apps, .. } = engine.query_center(query("apps")).await.unwrap() else {
        panic!()
    };
    assert_eq!(apps[0].count, 1);
    let mut request = query("app");
    request.app_key = Some(apps[0].key.clone());
    request.selected = Some(Position {
        created: apps[0].latest.created_unix_ms,
        id,
    });
    let mut incoming = notification("chat", "replacement");
    incoming.hints.transient = true;
    engine.notify(id, incoming).await.unwrap();
    let CenterPage::App {
        selected, count, ..
    } = engine.query_center(request.clone()).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(count, 1);
    assert_eq!(selected.unwrap().notification.summary, "replacement");
    engine.dismiss(id).await.unwrap();
    let CenterPage::App {
        selected, count, ..
    } = engine.query_center(request).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(count, 0);
    assert!(selected.is_none());
}
#[tokio::test]
async fn center_searches_full_unicode_body_and_resolves_legacy_group_links() {
    let dir = tempfile::tempdir().unwrap();
    let engine =
        NotificationEngine::persistent(StateStore::default(), dir.path().join("history.db"))
            .await
            .unwrap();
    let mut incoming = notification("chat", "Short preview");
    incoming.body = format!("{} ÄRGER %_", "x".repeat(1000));
    let id = engine.notify(0, incoming).await.unwrap();
    let group = engine.active().await[0].group_key.clone();
    engine.dismiss(id).await.unwrap();
    let mut request = query("apps");
    request.query = "ÄRGER %_".into();
    let CenterPage::Apps { apps, .. } = engine.query_center(request).await.unwrap() else {
        panic!()
    };
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].latest.body.chars().count(), 240);
    assert!(!apps[0].latest.body.contains("ÄRGER"));
    let mut request = query("app");
    request.group_key = Some(group);
    request.selected = Some(Position {
        created: apps[0].latest.created_unix_ms,
        id,
    });
    let CenterPage::App {
        app_key, selected, ..
    } = engine.query_center(request).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(app_key, "desktop:chat");
    assert!(selected.unwrap().notification.body.contains("ÄRGER"));
    assert_eq!(
        super::center::app_key(&"d".repeat(1025), "App", 7, 8),
        "unknown:8:7"
    );
    assert_ne!(
        super::center::app_key("", "a:b", 1, 2),
        super::center::app_key("", "a", 1, 2)
    );
}

#[tokio::test]
async fn center_ignores_notification_artwork_when_grouping_named_senders() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let engine = NotificationEngine::persistent(StateStore::default(), path.clone())
        .await
        .unwrap();
    for icon in ["battery-full-charged", "", "battery-caution"] {
        let mut incoming = notification("bar-daemon", "Battery event");
        incoming.hints.desktop_entry.clear();
        incoming.app_icon = icon.into();
        let id = engine.notify(0, incoming).await.unwrap();
        if !icon.is_empty() {
            engine.dismiss(id).await.unwrap();
        }
    }
    for desktop in ["first.app", "second.app"] {
        let mut incoming = notification("bar-daemon", "Distinct app");
        incoming.hints.desktop_entry = desktop.into();
        engine.notify(0, incoming).await.unwrap();
    }
    let CenterPage::Apps {
        apps, total_apps, ..
    } = engine.query_center(query("apps")).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(
        total_apps, 3,
        "distinct desktop IDs must not merge by display name"
    );
    let group = apps
        .iter()
        .find(|app| app.key == "named:bar-daemon")
        .unwrap();
    assert_eq!(group.count, 3, "mixed live/history icons share one app");
    let mut request = query("app");
    request.app_key = Some(group.key.clone());
    let CenterPage::App { count, entries, .. } =
        engine.query_center(request.clone()).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(count, 3);
    assert_eq!(entries.len(), 3);
    let restarted = NotificationEngine::persistent(StateStore::default(), path)
        .await
        .unwrap();
    let CenterPage::App { count, .. } = restarted.query_center(request).await.unwrap() else {
        panic!()
    };
    assert_eq!(count, 3, "persisted metadata uses the same identity policy");
}

#[tokio::test]
async fn center_keeps_artwork_hints_in_live_archived_and_restarted_previews() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let mut engine = NotificationEngine::persistent(StateStore::default(), path.clone())
        .await
        .unwrap();
    // Real sender shapes: artwork is frequently a hint, not app_icon.
    let cases = [
        ("Signal", "", "org.signal.Signal"),
        ("Thunderbird", "", "thunderbird"),
        ("", "com.mitchellh.ghostty", "com.mitchellh.ghostty"),
        ("Shelllist", "", "dialog-error"),
        ("bar-daemon", "", "battery-caution"),
        ("satty", "com.gabm.satty", "/icons/satty.svg"),
    ];
    let mut ids = Vec::new();
    for (name, desktop, image) in cases {
        let mut incoming = notification(name, "Artwork");
        incoming.hints.desktop_entry = desktop.into();
        incoming.hints.image_path = image.into();
        ids.push(engine.notify(0, incoming).await.unwrap());
    }
    for phase in 0..3 {
        let CenterPage::Apps { apps, .. } = engine.query_center(query("apps")).await.unwrap()
        else {
            panic!()
        };
        for ((_, desktop, image), id) in cases.iter().zip(&ids) {
            let app = apps.iter().find(|app| app.latest.id == *id).unwrap();
            let mut request = query("app");
            request.app_key = Some(app.key.clone());
            let CenterPage::App {
                entries, overview, ..
            } = engine.query_center(request).await.unwrap()
            else {
                panic!()
            };
            for preview in [&app.latest, &entries[0], &overview[0]] {
                let value = serde_json::to_value(preview).unwrap();
                assert_eq!(
                    value["hints"],
                    serde_json::json!({"desktop_entry": desktop, "image_path": image})
                );
                assert_eq!(value["app_icon"], "");
            }
        }
        if phase == 0 {
            for id in &ids {
                engine.dismiss(*id).await.unwrap();
            }
        } else if phase == 1 {
            engine = NotificationEngine::persistent(StateStore::default(), path.clone())
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn center_bounds_artwork_hints_for_live_and_stored_previews() {
    let dir = tempfile::tempdir().unwrap();
    let engine =
        NotificationEngine::persistent(StateStore::default(), dir.path().join("history.db"))
            .await
            .unwrap();
    let mut incoming = notification("Long hints", "Artwork");
    incoming.hints.desktop_entry = "界".repeat(1100);
    incoming.hints.image_path = "画".repeat(600);
    let id = engine.notify(0, incoming).await.unwrap();
    for archived in [false, true] {
        if archived {
            engine.dismiss(id).await.unwrap();
        }
        let CenterPage::Apps { apps, .. } = engine.query_center(query("apps")).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(apps[0].latest.hints.desktop_entry, "界".repeat(1024));
        assert_eq!(apps[0].latest.hints.image_path, "画".repeat(512));
    }
}

#[tokio::test]
async fn center_validates_queries_and_does_not_merge_unnamed_senders() {
    let engine = NotificationEngine::new(StateStore::default()).await;
    for value in [
        serde_json::json!({"view":"wrong"}),
        serde_json::json!({"view":"app","page":0}),
        serde_json::json!({"view":"app","query":"x".repeat(1025)}),
        serde_json::json!({"view":"app","selected":{"created":1,"id":0}}),
    ] {
        assert!(matches!(
            engine
                .query_center(serde_json::from_value(value).unwrap())
                .await,
            Err(HistoryError::Invalid)
        ));
    }
    let first = engine.notify(0, notification("", "first")).await.unwrap();
    let second = engine.notify(0, notification("", "second")).await.unwrap();
    assert_ne!(first, second);
    let CenterPage::Apps { total_apps, .. } = engine.query_center(query("apps")).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(total_apps, 2);
}
