//! Exercise the composition root without starting monitors or contacting host services.
use super::{ActivityService, ApiService, MediaService, NotificationService, StateStore};
use crate::{activity::notifications::engine::NotificationEngine, protocol::stream};
use serde_json::{Value, json};

#[tokio::test]
async fn dispatch_preserves_projection_publication_validation_and_failure_fences() {
    let directory = tempfile::tempdir().unwrap();
    let state = StateStore::default();
    let activity = ActivityService::with_paths(
        state.clone(),
        directory.path().join("config.json"),
        directory.path().join("todos.json"),
    )
    .await;
    let notifications = NotificationService::native(NotificationEngine::new(state.clone()).await);
    let api = ApiService::new(
        state.clone(),
        activity,
        notifications,
        MediaService::default(),
        crate::brightness::BrightnessService::new(state.clone()),
    );
    let full = api.dispatch("bar.snapshot", json!({})).await;
    assert_eq!(
        full,
        crate::protocol::read_wire::snapshot(&state.snapshot().await)
    );
    for streams in [
        json!([]),
        json!([stream::THEME]),
        json!([stream::THEME, stream::ACTIVITY, stream::THEME]),
    ] {
        let expected: serde_json::Map<String, Value> = streams
            .as_array()
            .unwrap()
            .iter()
            .map(|name| {
                let spec = crate::protocol::read_wire::stream_spec(name.as_str().unwrap()).unwrap();
                (
                    spec.snapshot_key.into(),
                    full["data"]["snapshot"][spec.snapshot_key].clone(),
                )
            })
            .collect();
        assert_eq!(
            api.dispatch("bar.snapshot", json!({"streams":streams}))
                .await,
            crate::protocol::success(json!({"snapshot":expected}))
        );
    }
    for streams in [
        Value::Null,
        json!(stream::THEME),
        json!([1]),
        json!(["unknown"]),
    ] {
        assert_eq!(
            api.dispatch("bar.snapshot", json!({"streams":streams}))
                .await["error"]["code"],
            "validation-error"
        );
    }
    let mut events = state.subscribe_domains(&[stream::THEME]);
    let params = json!({"seed":"#008577", "dark":true});
    let resolved = api.dispatch("theme.resolve", params.clone()).await;
    assert_eq!(resolved["ok"], true);
    assert_eq!(
        api.dispatch("bar.snapshot", json!({})).await,
        full,
        "resolve has no state effect"
    );
    assert!(events.try_recv().is_err());
    let configured = api.dispatch("theme.configure", params).await;
    assert_eq!(configured["data"]["theme"], resolved["data"]["theme"]);
    assert!(configured["data"]["theme"].get("_publication").is_none());
    assert_eq!(
        events.try_recv().unwrap().data,
        configured["data"]["snapshot"]["theme"]
    );
    assert_eq!(
        api.dispatch("bar.snapshot", json!({"streams":[stream::THEME]}))
            .await["data"]["snapshot"]["theme"],
        configured["data"]["snapshot"]["theme"]
    );
    for params in [
        Value::Null,
        json!({"seed":"red", "dark":false}),
        json!({"seed":"#ffffff", "dark":1}),
    ] {
        assert_eq!(api.dispatch("theme.configure", params).await["ok"], false);
        assert!(
            events.try_recv().is_err(),
            "invalid themes must not publish"
        );
    }
    for method in [
        "unknown",
        "activity.unknown",
        "locations.unknown",
        "location.unknown",
        "todos.unknown",
        "battery.unknown",
        "notifications.unknown",
        "audio.unknown",
        "powerSleep.unknown",
        "updates.unknown",
    ] {
        assert_eq!(
            api.dispatch(method, json!({})).await,
            crate::protocol::unsupported_method(method)
        );
    }
    assert_eq!(
        api.dispatch("todos.create", json!({"title":"Local only"}))
            .await["ok"],
        true
    );
    assert_eq!(
        api.dispatch("notifications.setDnd", json!({"enabled":true}))
            .await["data"]["notifications"]["dnd"],
        true
    );
    state
        .monitor_failed("activity", &[stream::ACTIVITY], "test failure")
        .await;
    for params in [json!({}), json!({"streams":[stream::ACTIVITY]})] {
        let result = api.dispatch("bar.snapshot", params).await;
        assert_eq!(result["data"]["snapshot"]["activity"]["available"], false);
        assert!(
            result["data"]["snapshot"]["activity"]["error"]
                .as_str()
                .unwrap()
                .contains("test failure")
        );
    }
    assert_eq!(
        api.dispatch("activity.refresh", json!({})).await["data"]["activity"]["available"],
        false
    );
    assert_eq!(
        serde_json::to_value(state.read(|s| s.theme.clone()).await).unwrap(),
        resolved["data"]["theme"]
    );
}
