use super::ActivityApi;
use crate::{activity::ActivityService, state::StateStore};
use serde_json::{Value, json};

#[tokio::test]
async fn real_agenda_api_crud_receipts_conflicts_and_legacy_adapter_share_one_store() {
    let dir = tempfile::tempdir().unwrap();
    let state = StateStore::default();
    let service = ActivityService::with_paths(
        state.clone(),
        dir.path().join("config.json"),
        dir.path().join("todos.json"),
    )
    .await;
    // Exercise the real top-level bar dispatcher, not only the domain adapter.
    // Unused service constructors are inert; the isolated test never starts
    // their monitors or contacts desktop/system services.
    let api = crate::api::ApiService::new(
        state.clone(),
        service.clone(),
        crate::activity::notifications::service::NotificationService::swaync(),
        crate::media::MediaService::default(),
        crate::brightness::BrightnessService::new(state.clone()),
    );
    let mut agenda_view = state.subscribe_domains(&[crate::protocol::stream::ACTIVITY]);
    let mut calendar_view = state.subscribe_domains(&[crate::protocol::stream::ACTIVITY]);
    let created = api.dispatch("agenda.create",json!({"request_id":"task-1","item":{"kind":"task","title":"Write release","task":{"priority":7,"checklist":[{"id":"step","text":"Run tests","checked":false}]}}})).await;
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(
        agenda_view.recv().await.unwrap().stream,
        crate::protocol::stream::ACTIVITY
    );
    assert_eq!(
        calendar_view.recv().await.unwrap().stream,
        crate::protocol::stream::ACTIVITY
    );
    let item = &created["data"]["item"];
    let id = item["id"].as_str().unwrap();
    assert_eq!(item["task"]["priority"], 7);
    let receipt = api
        .dispatch("agenda.receipt", json!({"request_id":"task-1"}))
        .await;
    assert_eq!(receipt["data"]["receipt"], created["data"]);
    let duplicate = api.dispatch("agenda.create",json!({"request_id":"task-1","item":{"kind":"task","title":"Write release","task":{"priority":7,"checklist":[{"id":"step","text":"Run tests","checked":false}]}}})).await;
    assert_eq!(duplicate, created);
    assert!(
        agenda_view.try_recv().is_err(),
        "duplicate receipt emits no false new revision"
    );
    let completed = api.dispatch("todos.complete", json!({"id":id})).await;
    assert_eq!(completed["ok"], true, "{completed}");
    let read = api.dispatch("agenda.get", json!({"id":id})).await;
    assert_eq!(read["data"]["item"]["task"]["completed"], true);
    assert_eq!(
        read["data"]["item"]["task"]["checklist"][0]["checked"],
        false
    );
    assert_eq!(read["data"]["item"]["schedule"], item["schedule"]);
    assert!(!dir.path().join("todos.json").exists());
    let revision = read["data"]["item"]["revision"].as_u64().unwrap();
    let changed = api.dispatch("agenda.patch",json!({"request_id":"notes-1","id":id,"expected_revision":revision,"patch":{"notes":"Native searchable needle"}})).await;
    assert_eq!(changed["ok"], true, "{changed}");
    let conflict = api.dispatch("agenda.patch",json!({"request_id":"conflict","id":id,"expected_revision":revision,"patch":{"title":"Lost edit"}})).await;
    assert_eq!(conflict["ok"], false);
    let found = api
        .dispatch("agenda.list", json!({"query":"needle","limit":1}))
        .await;
    assert_eq!(found["data"]["items"][0]["id"], id);
    let event = api
        .dispatch(
            "agenda.create",
            json!({"request_id":"event-1","item":{"kind":"event","title":"Unscheduled event"}}),
        )
        .await;
    assert_eq!(event["ok"], true, "{event}");
    assert_eq!(state.read(|s| s.activity.event_count).await, 1);
    assert_eq!(event["data"]["item"]["capabilities"]["complete"], false);
    let event_id = event["data"]["item"]["id"].as_str().unwrap();
    assert_eq!(
        api.dispatch("todos.complete", json!({"id":event_id})).await["ok"],
        false
    );
    let failed = api.dispatch("agenda.patch",json!({"request_id":"invalid-range","id":id,"expected_revision":revision+1,"patch":{"title":"Must not persist","schedule":{"mode":"date","start_date":"2026-10-29","end_date":"2026-10-27"}}})).await;
    assert_eq!(failed["ok"], false);
    assert_eq!(
        api.dispatch("agenda.get", json!({"id":id})).await["data"]["item"]["title"],
        "Write release"
    );
    let deleted = api
        .dispatch(
            "agenda.delete",
            json!({"request_id":"delete-1","id":id,"expected_revision":revision+1}),
        )
        .await;
    assert_eq!(deleted["ok"], true, "{deleted}");
    assert_eq!(deleted["data"]["item"], Value::Null);
    assert_eq!(
        api.dispatch("agenda.receipt", json!({"request_id":"delete-1"}))
            .await["data"]["receipt"],
        deleted["data"]
    );
    assert_eq!(
        api.dispatch("agenda.get", json!({"id":id})).await["ok"],
        false
    );
    assert_eq!(
        api.dispatch("agenda.list", json!({})).await["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(service.data_snapshot_for_test().await.items.len(), 1);
    for method in [
        "agenda.list",
        "agenda.get",
        "agenda.create",
        "agenda.patch",
        "agenda.delete",
        "agenda.markers",
        "agenda.receipt",
        "agenda.schedule",
    ] {
        assert!(crate::protocol::METHODS.contains(&method));
        assert!(
            crate::protocol::registry()["methods"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["name"] == method)
        );
        assert_eq!(
            api.dispatch(method, Value::Null).await["ok"],
            false,
            "{method}"
        );
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn read_only_ics_conversion_markers_native_scope_and_recurrence_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.ics");
    std::fs::write(&path,"BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:trip\nSUMMARY:Trip\nDTSTART;VALUE=DATE:20261231\nDTEND;VALUE=DATE:20270102\nEND:VEVENT\nEND:VCALENDAR\n").unwrap();
    std::fs::write(
        dir.path().join("config.json"),
        json!({"calendar_sources":[{"id":"travel","name":"旅行".repeat(2000),"kind":"ics-file","path":path}]}).to_string(),
    )
    .unwrap();
    let state = StateStore::default();
    let service = ActivityService::with_paths(
        state.clone(),
        dir.path().join("config.json"),
        dir.path().join("todos.json"),
    )
    .await;
    service.refresh().await;
    let api = ActivityApi::new(state.clone(), service.clone());
    let items = api
        .dispatch("agenda.list", json!({"day":"2027-01-01"}))
        .await;
    assert_eq!(items["ok"], true, "{items}");
    assert_eq!(items["data"]["failing_source_count"], 0);
    assert_eq!(items["data"]["source_error"], Value::Null);
    let item = &items["data"]["items"][0];
    assert_eq!(
        item["schedule"]["end_date"], "2027-01-01",
        "exclusive ICS end converted once"
    );
    assert_eq!(
        item["capabilities"],
        json!({"write":false,"delete":false,"complete":false})
    );
    let markers = api
        .dispatch(
            "agenda.markers",
            json!({"from_date":"2026-12-30","to_date":"2027-01-02"}),
        )
        .await;
    assert_eq!(
        markers["data"]["days"],
        json!([
            {"date":"2026-12-30","event":false,"task":false},{"date":"2026-12-31","event":true,"task":false},
            {"date":"2027-01-01","event":true,"task":false},{"date":"2027-01-02","event":false,"task":false}
        ])
    );
    for method in ["agenda.patch", "agenda.delete"] {
        let mut params = json!({"request_id":method,"id":item["id"],"expected_revision":1});
        if method.ends_with("patch") {
            params["patch"] = json!({"title":"Do not write"});
        }
        assert_eq!(api.dispatch(method, params).await["ok"], false);
    }
    let before = state.read(|s| s.activity.agenda.clone()).await;
    service.refresh().await;
    assert_eq!(before, state.read(|s| s.activity.agenda.clone()).await);
    assert_eq!(item["source_uid"], "trip");
    let local = api.dispatch("agenda.create",json!({"request_id":"local-alongside-import","item":{"kind":"task","title":"Unscheduled"}})).await;
    assert_eq!(local["ok"], true);
    let old_page = api.dispatch("agenda.list", json!({"limit":1})).await;
    assert!(old_page["data"]["next_cursor"].is_string());
    let content = std::fs::read_to_string(&path)
        .unwrap()
        .replace("SUMMARY:Trip", "SUMMARY:Changed trip")
        .replace("DTSTART;VALUE=DATE:20261231", "DTSTART;VALUE=DATE:20270101")
        .replace("DTEND;VALUE=DATE:20270102", "DTEND;VALUE=DATE:20270103");
    std::fs::write(&path, &content).unwrap();
    service.refresh().await;
    let changed = api
        .dispatch("agenda.list", json!({"day":"2027-01-01"}))
        .await;
    let accepted = changed["data"]["items"][0].clone();
    assert_eq!(accepted["title"], "Changed trip");
    assert_eq!(
        accepted["id"], item["id"],
        "source + UID identity survives rescheduling"
    );
    assert_eq!(
        api.dispatch("agenda.get", json!({"id":item["id"]})).await["data"]["item"]["schedule"]["start_date"],
        "2027-01-01"
    );
    assert!(
        changed["data"]["catalog_revision"].as_u64().unwrap()
            > old_page["data"]["catalog_revision"].as_u64().unwrap()
    );
    assert!(accepted["revision"].as_u64().unwrap() > item["revision"].as_u64().unwrap());
    let stale = api
        .dispatch(
            "agenda.list",
            json!({"limit":1,"cursor":old_page["data"]["next_cursor"]}),
        )
        .await;
    assert_eq!(stale["ok"], false, "changed import fences old pagination");
    assert_eq!(
        api.dispatch(
            "agenda.receipt",
            json!({"request_id":"local-alongside-import"})
        )
        .await["data"]["receipt"],
        local["data"],
        "source revision changes never rewrite mutation receipts"
    );
    let content = content.replace(
        "SUMMARY:Changed trip",
        "SUMMARY:Changed trip\nRRULE:FREQ=DAILY",
    );
    std::fs::write(&path, content).unwrap();
    service.refresh().await;
    assert!(
        state
            .read(|s| s.activity.sources[0]
                .error
                .as_ref()
                .unwrap()
                .contains("unsupported iCalendar recurrence"))
            .await
    );
    assert_eq!(
        api.dispatch("agenda.list", json!({"day":"2027-01-01"}))
            .await["data"]["items"][0],
        accepted,
        "retain accepted import after provider failure"
    );
    let warning = api.dispatch("agenda.list", json!({})).await;
    assert_eq!(
        warning["ok"], true,
        "source failure is not a failed local catalog read"
    );
    assert_eq!(warning["data"]["failing_source_count"], 1);
    let diagnostic = warning["data"]["source_error"].as_str().unwrap();
    assert!(diagnostic.len() <= 1024);
    assert!(diagnostic.starts_with("unsupported iCalendar recurrence"));
    assert!(diagnostic.contains("旅行"));
    assert!(diagnostic.contains("unsupported iCalendar recurrence"));
    assert_eq!(
        warning["data"]["catalog_revision"], changed["data"]["catalog_revision"],
        "health-only refresh retains the same usable catalog revision"
    );
    let local_id = local["data"]["item"]["id"].as_str().unwrap();
    assert!(
        warning["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["id"] == local_id && record["capabilities"]["write"] == true)
    );
    let marker_warning = api
        .dispatch(
            "agenda.markers",
            json!({"from_date":"2027-01-01","to_date":"2027-01-01"}),
        )
        .await;
    assert_eq!(marker_warning["ok"], true);
    assert_eq!(
        marker_warning["data"]["source_error"],
        warning["data"]["source_error"]
    );
    assert_eq!(marker_warning["data"]["failing_source_count"], 1);
    assert_eq!(
        marker_warning["data"]["days"][0]["event"], true,
        "marker-only first open retains accepted membership beside its warning"
    );
    let mut task = local["data"]["item"]["task"].clone();
    task["completed"] = json!(true);
    assert_eq!(
        api.dispatch("agenda.patch", json!({"request_id":"complete-with-source-warning","id":local_id,"expected_revision":local["data"]["item"]["revision"],"patch":{"task":task}})).await["ok"],
        true
    );
    assert_eq!(
        api.dispatch("agenda.get", json!({"id":local_id})).await["data"]["item"]["task"]["completed"],
        true,
        "nonfatal provider warnings never fence local mutations"
    );
    std::fs::write(dir.path().join("config.json"), "{}").unwrap();
    service.refresh().await;
    assert!(
        api.dispatch("agenda.list", json!({"day":"2027-01-01"}))
            .await["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        api.dispatch("agenda.get", json!({"id":item["id"]})).await["ok"],
        false
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn actual_migration_and_database_failures_reach_agenda_reads_without_private_input_values() {
    for bad_legacy in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let legacy_path = dir.path().join("todos.json");
        let preserved_path = if bad_legacy {
            std::fs::write(&legacy_path,json!([{"id":"old-task","source_id":"local","title":"Keep private","completed":false,"priority":"PRIVATE_LEGACY_CONTENT","due_unix_ms":null,"due_date":null,"created_unix_ms":1,"completed_unix_ms":null}]).to_string()).unwrap();
            legacy_path.clone()
        } else {
            std::fs::create_dir(dir.path().join("agenda")).unwrap();
            let path = dir.path().join("agenda/agenda.db");
            std::fs::write(&path, b"not a database: PRIVATE_DATABASE_CONTENT").unwrap();
            path
        };
        let original = std::fs::read(&preserved_path).unwrap();
        let state = StateStore::default();
        let service =
            ActivityService::with_paths(state.clone(), dir.path().join("config.json"), legacy_path)
                .await;
        let api = ActivityApi::new(state, service.clone());
        for (method, params) in [
            ("agenda.list", json!({})),
            (
                "agenda.markers",
                json!({"from_date":"2027-01-01","to_date":"2027-01-01"}),
            ),
            ("agenda.get", json!({"id":"old-task"})),
            ("agenda.receipt", json!({"request_id":"unknown"})),
        ] {
            let reply = api.dispatch(method, params).await;
            assert_eq!(reply["ok"], false, "{reply}");
            let message = reply["error"]["message"].as_str().unwrap();
            assert!(message.len() <= 1024);
            assert!(
                !message.contains("PRIVATE_"),
                "never echo malformed input values: {message}"
            );
            if bad_legacy {
                assert!(message.contains("legacy todo migration"), "{message}");
                assert!(
                    message.contains("JSON Data at line"),
                    "typed parser cause/location survives: {message}"
                );
            } else {
                assert!(message.contains("database"), "{message}");
                assert_ne!(message, "Agenda store unavailable");
            }
        }
        assert_eq!(std::fs::read(preserved_path).unwrap(), original);
        service.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn abandoned_transport_waiter_cannot_cancel_durable_publication_or_duplicate_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let state = StateStore::default();
    let service = ActivityService::with_paths(
        state.clone(),
        dir.path().join("config.json"),
        dir.path().join("todos.json"),
    )
    .await;
    let api = ActivityApi::new(state.clone(), service.clone());
    let mut agenda_view = state.subscribe_domains(&[crate::protocol::stream::ACTIVITY]);
    let mut calendar_view = state.subscribe_domains(&[crate::protocol::stream::ACTIVITY]);
    let mut waiter = Box::pin(api.dispatch("agenda.create",json!({"request_id":"abandoned-create","item":{"kind":"task","title":"Committed after disconnect"}})));
    assert!(futures::poll!(waiter.as_mut()).is_pending());
    // Drop the actual dispatch waiter before the owned mutation task can run.
    // Its independently owned task must still commit and notify both views.
    drop(waiter);
    tokio::time::timeout(std::time::Duration::from_secs(3), agenda_view.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), calendar_view.recv())
        .await
        .unwrap()
        .unwrap();
    let receipt = api
        .dispatch("agenda.receipt", json!({"request_id":"abandoned-create"}))
        .await;
    assert_eq!(
        receipt["data"]["receipt"]["item"]["title"],
        "Committed after disconnect"
    );
    let catalog = api.dispatch("agenda.list", json!({})).await;
    assert_eq!(catalog["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        catalog["data"]["items"][0]["id"],
        receipt["data"]["receipt"]["item"]["id"]
    );
    assert_eq!(service.data_snapshot_for_test().await.items.len(), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn schedule_api_validates_wall_time_and_native_calendar_scope() {
    let dir = tempfile::tempdir().unwrap();
    let state = StateStore::default();
    let service = ActivityService::with_paths(
        state.clone(),
        dir.path().join("config.json"),
        dir.path().join("todos.json"),
    )
    .await;
    let api = ActivityApi::new(state, service);
    let request =
        |text| json!({"mode":"timed","start_text":text,"end_text":"","timezone":"Europe/Berlin"});
    assert_eq!(
        api.dispatch("agenda.schedule", request("2026-03-29T02:30:00+01:00"))
            .await["ok"],
        false
    );
    let valid = api
        .dispatch("agenda.schedule", request("2026-10-25T02:30:00+01:00"))
        .await;
    assert_eq!(valid["ok"], true, "{valid}");
    assert!(valid["data"]["schedule"]["start_unix_ms"].is_i64());
    assert_eq!(valid["data"]["schedule"]["end_unix_ms"], Value::Null);
    let empty = api
        .dispatch(
            "agenda.schedule",
            json!({"mode":"date","start_text":"","end_text":"","timezone":""}),
        )
        .await;
    assert_eq!(empty["ok"], true);
    assert_eq!(empty["data"]["schedule"]["start_date"], Value::Null);
    api.activity.shutdown().await.unwrap();
}
