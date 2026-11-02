use super::*;
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    root: tempfile::TempDir,
    service: Arc<Service>,
    state: Arc<Mutex<Value>>,
    requests: Arc<Mutex<Vec<String>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("hypr/test");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let listener = tokio::net::UnixListener::bind(directory.join(".socket.sock")).unwrap();
        let state = Arc::new(Mutex::new(json!({
            "monitors":[{"name":"DP-1", "focused":true,"activeWorkspace":{"id":1}}],
            "workspaces":[{"id":1,"monitor":"DP-1","windows":0}], "windows":[]
        })));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shared = state.clone();
        let calls = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut command = String::new();
                stream.read_to_string(&mut command).await.unwrap();
                calls.lock().await.push(command.clone());
                let reply = if command.starts_with("[[BATCH]]") {
                    let state = shared.lock().await;
                    format!(
                        "{}\n\n{}\n\n{}",
                        state["monitors"], state["workspaces"], state["windows"]
                    )
                } else {
                    "ok".into()
                };
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        let service = Self::service(root.path());
        Self {
            root,
            service,
            state,
            requests,
            server,
        }
    }
    fn service(root: &std::path::Path) -> Arc<Service> {
        Arc::new(Service {
            client: Client::new(root.into(), Some("test".into())),
            journal_root: Some(root.into()),
            ..Service::default()
        })
    }
    fn request(key: &str) -> Create {
        Create {
            request_id: key.into(),
            context: Context {
                session: "test".into(),
                monitor: "DP-1".into(),
                workspace_id: 1,
            },
            purpose: Purpose::Launch,
            workspace_id: None,
            origin: "Code".into(),
            icon: "code".into(),
        }
    }
    async fn create(&self, key: &str) -> Value {
        self.service
            .call(
                "workspace.create",
                serde_json::to_value(Self::request(key)).unwrap(),
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn concurrent_reservations_are_unique_and_duplicate_keys_never_reallocate() {
    let fixture = Fixture::new().await;
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..12 {
        let service = fixture.service.clone();
        tasks.spawn(async move {
            service
                .call(
                    "workspace.create",
                    serde_json::to_value(Fixture::request(&format!("create-{index}"))).unwrap(),
                )
                .await
                .unwrap()
        });
    }
    let mut ids = Vec::new();
    while let Some(result) = tasks.join_next().await {
        ids.push(result.unwrap()["lease"]["workspace_id"].as_i64().unwrap());
    }
    ids.sort_unstable();
    assert_eq!(ids, (6..18).collect::<Vec<_>>());
    let before = fixture.create("create-0").await;
    assert_eq!(fixture.create("create-0").await, before);
    let mut changed = Fixture::request("create-0");
    changed.origin = "Other".into();
    assert!(
        fixture
            .service
            .call("workspace.create", serde_json::to_value(changed).unwrap())
            .await
            .is_err()
    );
    let restored = Fixture::service(fixture.root.path());
    let after = restored
        .call(
            "workspace.create",
            serde_json::to_value(Fixture::request("next")).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(after["lease"]["workspace_id"], 18);
}

#[tokio::test]
async fn restart_fences_old_placement_and_lookup_retains_indeterminate_receipt() {
    let fixture = Fixture::new().await;
    let reserved = fixture.create("before-crash").await;
    let lease: Lease = serde_json::from_value(reserved["lease"].clone()).unwrap();
    let restored = Fixture::service(fixture.root.path());
    let status = restored
        .call(
            "workspace.operation.get",
            json!({"session":"test","request_id":"before-crash"}),
        )
        .await
        .unwrap();
    assert_eq!(status["status"], "unknown");
    assert_eq!(status["released"], true);
    let placement = Placement {
        lease: lease.clone(),
        window: wire::WindowIdentity {
            address: "0x123".into(),
            pid: 1,
            workspace_id: 1,
        },
        follow: true,
    };
    assert!(
        restored
            .call(
                "workspace.placement.apply",
                serde_json::to_value(placement).unwrap()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("fenced")
    );
    restored
        .call(
            "workspace.lease.release",
            serde_json::to_value(lease).unwrap(),
        )
        .await
        .unwrap();
    assert!(
        fixture
            .requests
            .lock()
            .await
            .iter()
            .all(|r| r.starts_with("[[BATCH]]"))
    );
}

#[tokio::test]
async fn lost_close_reply_recovers_without_dispatching_again_or_sweeping_new_arrivals() {
    let fixture = Fixture::new().await;
    fixture.state.lock().await["windows"] =
        json!([{"address":"0x123","pid":42,"workspace":{"id":1},"mapped":true}]);
    let request = json!({"request_id":"close-1","context":{"session":"test","monitor":"DP-1","workspace_id":1}});
    let admitted = fixture
        .service
        .call("workspace.close", request.clone())
        .await
        .unwrap();
    assert_eq!(admitted["targets"].as_array().unwrap().len(), 1);
    assert_eq!(admitted["targets"][0]["attempted"], true);
    fixture.state.lock().await["windows"] =
        json!([{"address":"0x456","pid":42,"workspace":{"id":1},"mapped":true}]);
    fixture
        .service
        .call("workspace.close", request)
        .await
        .unwrap();
    fixture.service.observe_close("close-1").await;
    let result = fixture
        .service
        .call(
            "workspace.operation.get",
            json!({"session":"test","request_id":"close-1"}),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "closed");
    let requests = fixture.requests.lock().await;
    assert_eq!(
        requests
            .iter()
            .filter(|s| s.contains("hl.dsp.window.close"))
            .count(),
        1
    );
    assert!(
        !requests
            .iter()
            .any(|s| s.contains("0x456") || s.contains("kill"))
    );
}

#[tokio::test]
async fn journal_failure_poisoning_prevents_effects_and_stale_context_is_rejected() {
    let fixture = Fixture::new().await;
    fixture
        .service
        .call("workspace.context", json!({}))
        .await
        .unwrap();
    let mut request = Fixture::request("stale");
    request.context.workspace_id = 8;
    assert!(
        fixture
            .service
            .call("workspace.create", serde_json::to_value(request).unwrap())
            .await
            .is_err()
    );
    let directory = fixture.root.path().join("bar-daemon");
    std::fs::rename(&directory, fixture.root.path().join("saved")).unwrap();
    std::fs::write(&directory, b"not a directory").unwrap();
    assert!(
        fixture
            .service
            .call(
                "workspace.create",
                serde_json::to_value(Fixture::request("failure")).unwrap()
            )
            .await
            .is_err()
    );
    std::fs::remove_file(&directory).unwrap();
    std::fs::rename(fixture.root.path().join("saved"), &directory).unwrap();
    assert!(
        fixture
            .service
            .call(
                "workspace.create",
                serde_json::to_value(Fixture::request("poisoned")).unwrap()
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .requests
            .lock()
            .await
            .iter()
            .all(|r| r.starts_with("[[BATCH]]"))
    );
}

#[tokio::test]
async fn user_adoption_and_unknown_observations_protect_empty_reservations() {
    let fixture = Fixture::new().await;
    let reserved = fixture.create("adopt").await;
    {
        let mut state = fixture.state.lock().await;
        state["monitors"][0]["activeWorkspace"]["id"] = 6.into();
        state["workspaces"] = json!([{"id":6,"monitor":"DP-1","windows":0}]);
    }
    fixture
        .service
        .project(&mut crate::model::WorkspaceState::default())
        .await;
    fixture
        .service
        .call("workspace.lease.release", reserved["lease"].clone())
        .await
        .unwrap();
    assert!(
        fixture
            .service
            .loaded
            .lock()
            .await
            .as_ref()
            .unwrap()
            .journal
            .entries[&6]
            .landing_pad
    );
    fixture.state.lock().await["windows"] = json!("unavailable");
    let mut projection = crate::model::WorkspaceState::default();
    fixture.service.project(&mut projection).await;
    assert!(!projection.lifecycle.available);
    assert!(
        fixture
            .service
            .loaded
            .lock()
            .await
            .as_ref()
            .unwrap()
            .journal
            .entries
            .contains_key(&6)
    );
    {
        let mut state = fixture.state.lock().await;
        state["windows"] = json!([]);
        state["monitors"][0]["activeWorkspace"]["id"] = 1.into();
    }
    fixture.service.project(&mut projection).await;
    assert!(
        !fixture
            .service
            .loaded
            .lock()
            .await
            .as_ref()
            .unwrap()
            .journal
            .entries
            .contains_key(&6)
    );
    assert!(
        fixture
            .requests
            .lock()
            .await
            .iter()
            .all(|r| r.starts_with("[[BATCH]]"))
    );
}

#[tokio::test]
async fn an_unobserved_fallback_never_retires_the_active_workspace() {
    let fixture = Fixture::new().await;
    let reserved = fixture.create("populated").await;
    {
        let mut state = fixture.state.lock().await;
        state["windows"] = json!([{"address":"0x123","pid":42,"workspace":{"id":6},"mapped":true}]);
        state["workspaces"] = json!([{"id":6,"monitor":"DP-1","windows":1}]);
        state["monitors"][0]["activeWorkspace"]["id"] = 6.into();
    }
    fixture
        .service
        .call("workspace.lease.release", reserved["lease"].clone())
        .await
        .unwrap();
    fixture.state.lock().await["windows"] = json!([]);
    // This fixture deliberately acknowledges focus without applying it.
    let mut projection = crate::model::WorkspaceState::default();
    fixture.service.project(&mut projection).await;
    assert!(projection.lifecycle.error.unwrap().contains("not observed"));
    assert!(
        fixture
            .service
            .loaded
            .lock()
            .await
            .as_ref()
            .unwrap()
            .journal
            .entries
            .contains_key(&6)
    );
}
