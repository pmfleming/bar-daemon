use super::{
    center::{CenterPage, CenterQuery},
    engine::NotificationEngine,
    history::Position,
    model::IncomingNotification,
};

fn query(view: &str) -> CenterQuery {
    serde_json::from_value(serde_json::json!({"view": view})).unwrap()
}
fn notification(app: &str, summary: &str) -> IncomingNotification {
    let mut n = super::model::incoming(app, summary, "A body searchable beyond the first page");
    n.hints.desktop_entry = app.into();
    n
}
#[tokio::test]
async fn center_groups_full_scope_and_seeks_without_loading_intermediate_bodies() {
    let (_dir, _path, engine) = NotificationEngine::persistent_fixture().await;
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
async fn center_deduplicates_live_and_never_resurrects_transient_replacement() {
    let (_dir, _path, engine) = NotificationEngine::persistent_fixture().await;
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
