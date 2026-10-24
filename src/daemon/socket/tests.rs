//! Real listener/D-Bus parity without starting monitors or touching host services.
use super::*;
use crate::{
    activity::{
        ActivityService,
        notifications::{engine::NotificationEngine, service::NotificationService},
    },
    api::ApiService,
    media::MediaService,
    state::StateStore,
};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::{Duration, timeout},
};
const DEADLINE: Duration = Duration::from_secs(5);
async fn request(peer: &mut BufReader<UnixStream>, value: Value, events: &mut Vec<Value>) -> Value {
    peer.get_mut()
        .write_all(format!("{value}\n").as_bytes())
        .await
        .unwrap();
    loop {
        let mut line = String::new();
        assert!(
            timeout(DEADLINE, peer.read_line(&mut line))
                .await
                .unwrap()
                .unwrap()
                > 0
        );
        let record: Value = serde_json::from_str(&line).unwrap();
        if record["kind"] == "response" && record["id"] == value["id"] {
            return record["response"].clone();
        }
        assert_eq!(record["kind"], "event", "{record}");
        assert!(events.len() < 64);
        events.push(record);
    }
}
async fn demand(state: &StateStore, expected: usize) {
    timeout(DEADLINE, async {
        loop {
            if *state.work_area_demand().borrow() == expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn direct_dispatch_mixed_streams_and_connection_lifetimes_share_the_service() {
    let directory = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = StateStore::default();
    let activity = ActivityService::with_paths(
        state.clone(),
        directory.path().join("config.json"),
        directory.path().join("todos.json"),
    )
    .await;
    let api = ApiService::new(
        state.clone(),
        activity,
        NotificationService::native(NotificationEngine::new(state.clone()).await),
        MediaService::default(),
        crate::brightness::BrightnessService::new(state.clone()),
    );
    let daemon = BarDaemon::new(api, state.clone());
    let subscriptions = daemon.subscriptions.clone();
    let bus = crate::battery::helper::authorization_tests::Bus::new();
    let server = bus.connection().await;
    server.request_name(crate::api::BUS_NAME).await.unwrap();
    server
        .object_server()
        .at(OBJECT_PATH, daemon)
        .await
        .unwrap();
    let client = bus.connection().await;
    let proxy = zbus::Proxy::new(
        &client,
        crate::api::BUS_NAME,
        OBJECT_PATH,
        crate::api::INTERFACE,
    )
    .await
    .unwrap();
    let listener = JsonlListener::bind(
        &std::fs::canonicalize(directory.path()).unwrap(),
        "bar-daemon",
    )
    .await
    .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(listener.serve(
        Arc::new(Service(server)),
        BasicCorrelation,
        JsonlSocketConfig::default(),
        async {
            let _ = stopped.await;
        },
    ));
    let mut a = BufReader::new(
        UnixStream::connect(directory.path().join("bar-daemon.sock"))
            .await
            .unwrap(),
    );
    let mut b = BufReader::new(
        UnixStream::connect(directory.path().join("bar-daemon.sock"))
            .await
            .unwrap(),
    );
    let mut events = Vec::new();
    for (method, params) in [
        (
            "bar.snapshot",
            json!({"streams":["audio.changed","battery.changed"]}),
        ),
        ("bar.snapshot", json!({"streams":[false]})),
        ("unknown.method", json!({})),
        (
            "battery.operation.status",
            json!({"operation_id":"unknown"}),
        ),
    ] {
        let dbus: String = proxy
            .call("Call", &(method, params.to_string()))
            .await
            .unwrap();
        let direct = request(
            &mut a,
            json!({"id":"read", "op":"call", "method":method, "params":params}),
            &mut events,
        )
        .await;
        assert_eq!(direct, serde_json::from_str::<Value>(&dbus).unwrap());
    }
    let owned = request(
        &mut a,
        json!({"id":"a-sub", "op":"subscribe", "streams":["audio.changed","workarea.changed"]}),
        &mut events,
    )
    .await;
    let foreign = owned["data"]["subscription"]["id"].as_str().unwrap();
    let other = request(
        &mut b,
        json!({"id":"b-sub", "op":"subscribe", "streams":["battery.changed","workarea.changed"]}),
        &mut events,
    )
    .await;
    demand(&state, 2).await;
    let response = request(
        &mut b,
        json!({"id":"foreign", "op":"cancel", "request_id":foreign}),
        &mut events,
    )
    .await;
    assert_eq!(response["error"]["code"], "request-not-found");
    events.clear();
    for n in 0..1024 {
        state
            .update_audio(crate::model::AudioState {
                available: true,
                volume_percent: (n % 100) as u8,
                ..Default::default()
            })
            .await;
    }
    state
        .update_battery(crate::model::BatteryState {
            available: true,
            percentage: 42,
            ..Default::default()
        })
        .await;
    let response = request(&mut b, json!({"id":"current", "op":"call", "method":"bar.snapshot", "params":{"streams":["battery.changed"]}}), &mut events).await;
    assert_eq!(response["data"]["snapshot"]["battery"]["percentage"], 42);
    assert!(events.iter().all(|e| e["stream"] != "audio.changed"));
    drop(a);
    demand(&state, 1).await;
    let response = request(
        &mut b,
        json!({"id":"cancel", "op":"cancel", "request_id":other["data"]["subscription"]["id"]}),
        &mut events,
    )
    .await;
    assert_eq!(response["ok"], true);
    demand(&state, 0).await;
    drop(b);
    let _ = stop.send(());
    timeout(DEADLINE, serving).await.unwrap().unwrap().unwrap();
    subscriptions.shutdown().await;
}
