use super::*;
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Housekeeping may forget only this owner's stale references. Safety assertions
// below continue to reject every window/workspace mutation.
fn reference_cleanup(request: &str) -> bool {
    request == format!("eval {}", compositor::release_prefix(PIN_PREFIX).unwrap())
}

#[tokio::test]
async fn reads_skip_capabilities_and_pin_cleanup_retries_before_admission() {
    let fixture = Fixture::new().await;
    fixture.state.lock().await["reject_cleanup"] = true.into();
    for method in ["workspace.context", "workspace.operation.get"] {
        let params = if method == "workspace.context" {
            json!({"monitor":"DP-1"})
        } else {
            json!({"request_id":"absent"})
        };
        // A missing operation is a domain error, not a capability error.
        let _ = fixture.service.call(method, params).await;
    }
    let mut state = crate::model::WorkspaceState {
        available: true,
        ..Default::default()
    };
    fixture.service.project(&mut state).await;
    assert!(state.lifecycle.error.is_none());
    assert_ne!(fixture.state.lock().await["capabilities_requested"], true);
    assert!(
        fixture
            .requests
            .lock()
            .await
            .iter()
            .all(|r| r.starts_with("[[BATCH]]j/"))
    );

    let request = json!({"request_id":"close", "context":Fixture::request("close").context});
    assert!(
        fixture
            .service
            .call("workspace.close", request.clone())
            .await
            .is_err()
    );
    assert!(
        !fixture
            .service
            .loaded
            .lock()
            .await
            .as_ref()
            .unwrap()
            .pins_cleaned
    );
    assert!(
        !fixture
            .requests
            .lock()
            .await
            .iter()
            .any(|r| r.starts_with("[[BATCH]]eval "))
    );
    fixture.state.lock().await["reject_cleanup"] = false.into();
    fixture
        .service
        .call("workspace.close", request)
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
            .pins_cleaned
    );
    let requests = fixture.requests.lock().await;
    let pin = requests
        .iter()
        .position(|r| r.starts_with("[[BATCH]]eval "))
        .unwrap();
    let cleanups: Vec<_> = requests
        .iter()
        .enumerate()
        .filter(|(_, r)| reference_cleanup(r))
        .collect();
    assert_eq!(cleanups.len(), 2);
    assert!(cleanups.iter().all(|(i, _)| *i < pin));
}

#[tokio::test]
async fn new_owner_cleans_once_before_first_handoff_pin() {
    let fixture = Fixture::new().await;
    fixture.tracked_launch().await;
    {
        let mut guard = fixture.service.loaded.lock().await;
        fixture
            .service
            .prepare_pins(guard.as_mut().unwrap())
            .await
            .unwrap();
    }
    assert_eq!(
        fixture
            .requests
            .lock()
            .await
            .iter()
            .filter(|r| reference_cleanup(r))
            .count(),
        1
    );
    let restarted = Fixture::service(fixture.root.path());
    restarted
        .call("workspace.context", json!({"monitor":"DP-1"}))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .requests
            .lock()
            .await
            .iter()
            .filter(|r| reference_cleanup(r))
            .count(),
        1
    );
    {
        let mut guard = restarted.loaded.lock().await;
        restarted
            .prepare_pins(guard.as_mut().unwrap())
            .await
            .unwrap();
    }
    assert_eq!(
        fixture
            .requests
            .lock()
            .await
            .iter()
            .filter(|r| reference_cleanup(r))
            .count(),
        2
    );
}

#[tokio::test]
async fn bar_positions_resolve_gaps_retained_entries_and_monitor_ownership() -> Result<()> {
    let fixture = Fixture::new().await;
    fixture
        .apply_focus
        .store(true, std::sync::atomic::Ordering::Relaxed);
    fixture.state.lock().await["workspaces"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!({"id":6,"monitor":"DP-1","windows":1}),
            json!({"id":7,"monitor":"other","windows":1}),
            json!({"id":10,"monitor":"DP-1","windows":1}),
            json!({"id":20,"monitor":"other","windows":1}),
        ]);
    for command in ["focus", "move"] {
        let reply = fixture
            .service
            .call(
                "workspace.command",
                json!({
                    "command":command,"workspace_position":7,"monitor":"DP-1"
                }),
            )
            .await
            .unwrap();
        assert_eq!(reply["workspace_id"], 10);
    }
    fixture.state.lock().await["workspaces"]
        .as_array_mut()
        .unwrap()
        .extend([30, 40, 50].map(|id| json!({"id":id,"monitor":"DP-1","windows":1})));
    for command in ["focus", "move"] {
        let reply = fixture
            .service
            .call(
                "workspace.command",
                json!({
                    "command":command,"workspace_position":10
                }),
            )
            .await
            .unwrap();
        assert_eq!(reply["workspace_id"], 50);
    }
    fixture.state.lock().await["workspaces"]
        .as_array_mut()
        .unwrap()
        .retain(|w| w["id"].as_i64().unwrap() < 30);
    // A closed/removed earlier entry shifts later shortcuts without renumbering IDs.
    fixture.state.lock().await["workspaces"]
        .as_array_mut()
        .unwrap()
        .retain(|w| w["id"] != 6);
    let reply = fixture
        .service
        .call(
            "workspace.command",
            json!({
                "command":"focus","workspace_position":6
            }),
        )
        .await
        .unwrap();
    assert_eq!(reply["workspace_id"], 10);
    let manual = fixture
        .service
        .call(
            "workspace.command",
            json!({
                "command":"create","request_id":"positional-manual"
            }),
        )
        .await
        .unwrap();
    let id = manual["lease"]["workspace_id"].as_i64().unwrap();
    fixture.state.lock().await["workspaces"]
        .as_array_mut()
        .unwrap()
        .retain(|w| w["id"] != id);
    for command in ["focus", "move"] {
        let reply = fixture
            .service
            .call(
                "workspace.command",
                json!({
                    "command":command,"workspace_position":7
                }),
            )
            .await
            .unwrap();
        assert_eq!(reply["workspace_id"], id);
    }
    // The bar always shows standard categories, but selection must not pull one
    // away from another monitor. A stale retained entry also follows live ownership.
    fixture.state.lock().await["workspaces"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!({"id":2,"monitor":"other","windows":1}),
            json!({"id":id,"monitor":"other","windows":0}),
        ]);
    let high = fixture
        .service
        .loaded
        .lock()
        .await
        .as_ref()
        .unwrap()
        .journal
        .high;
    for command in ["focus", "move"] {
        for position in [0, -1, 2, 7, 8, i64::MAX] {
            fixture.requests.lock().await.clear();
            assert!(
                fixture
                    .service
                    .call(
                        "workspace.command",
                        json!({
                            "command":command,"workspace_position":position
                        })
                    )
                    .await
                    .is_err()
            );
            assert!(
                !fixture
                    .requests
                    .lock()
                    .await
                    .iter()
                    .any(|s| s.starts_with("eval ") && !reference_cleanup(s))
            );
        }
        assert!(
            fixture
                .service
                .call(
                    "workspace.command",
                    json!({
                        "command":command,"workspace_position":1,"workspace_id":1
                    })
                )
                .await
                .is_err()
        );
    }
    // Cycling skips foreign standards and stale retained entries, in both directions.
    for (command, current, expected) in [("next", 1, 3), ("previous", 3, 1), ("next", 10, 1)] {
        fixture.state.lock().await["monitors"][0]["activeWorkspace"]["id"] = current.into();
        let reply = fixture
            .service
            .call("workspace.command", json!({"command":command}))
            .await?;
        assert_eq!(reply["workspace_id"], expected);
    }
    for command in ["create", "close", "next", "previous"] {
        assert!(
            fixture
                .service
                .call(
                    "workspace.command",
                    json!({
                        "command":command,"workspace_position":1
                    })
                )
                .await
                .is_err()
        );
    }
    assert_eq!(
        fixture
            .service
            .loaded
            .lock()
            .await
            .as_ref()
            .unwrap()
            .journal
            .high,
        high
    );
    Ok(())
}

#[tokio::test]
async fn numeric_commands_reject_absent_foreign_and_invalid_destinations_without_effects() {
    let fixture = Fixture::new().await;
    fixture.state.lock().await["workspaces"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":7,"monitor":"other","windows":1}));
    for command in ["focus", "move"] {
        for id in [0, -1, 6, 7, i64::MAX] {
            fixture.requests.lock().await.clear();
            assert!(
                fixture
                    .service
                    .call(
                        "workspace.command",
                        json!({"command":command,"workspace_id":id})
                    )
                    .await
                    .is_err()
            );
            assert!(
                !fixture
                    .requests
                    .lock()
                    .await
                    .iter()
                    .any(|s| s.starts_with("eval ") && !reference_cleanup(s))
            );
        }
        assert!(
            fixture
                .service
                .call("workspace.command", json!({"command":command}))
                .await
                .is_err()
        );
    }
    assert!(
        fixture
            .service
            .call(
                "workspace.command",
                json!({"command":"create","workspace_id":6})
            )
            .await
            .is_err()
    );
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
            .is_empty()
    );
}

#[tokio::test]
async fn numeric_commands_allow_standard_existing_and_retained_destinations() {
    let fixture = Fixture::new().await;
    fixture
        .apply_focus
        .store(true, std::sync::atomic::Ordering::Relaxed);
    fixture
        .service
        .call(
            "workspace.command",
            json!({"command":"focus","workspace_id":5}),
        )
        .await
        .unwrap();
    fixture.state.lock().await["workspaces"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":8,"monitor":"DP-1","windows":1}));
    fixture
        .service
        .call(
            "workspace.command",
            json!({"command":"focus","workspace_id":8}),
        )
        .await
        .unwrap();
    let created = fixture
        .service
        .call(
            "workspace.command",
            json!({"command":"create","request_id":"manual-numeric"}),
        )
        .await
        .unwrap();
    let id = created["lease"]["workspace_id"].as_i64().unwrap();
    fixture.state.lock().await["workspaces"]
        .as_array_mut()
        .unwrap()
        .retain(|w| w["id"] != id);
    fixture.state.lock().await["monitors"][0]["activeWorkspace"]["id"] = 1.into();
    for command in ["focus", "move"] {
        fixture
            .service
            .call(
                "workspace.command",
                json!({"command":command,"workspace_id":id}),
            )
            .await
            .unwrap();
    }
    let calls = fixture.requests.lock().await;
    assert!(
        calls
            .iter()
            .any(|c| c.contains("assert(target,'destination disappeared')"))
    );
    assert!(
        calls
            .iter()
            .any(|c| c.contains("follow=true") && c.contains("focused window changed"))
    );
}

struct Fixture {
    root: tempfile::TempDir,
    service: Arc<Service>,
    state: Arc<Mutex<Value>>,
    requests: Arc<Mutex<Vec<String>>>,
    apply_focus: Arc<std::sync::atomic::AtomicBool>,
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
            "monitors":[{"id":0,"name":"DP-1", "focused":true,"activeWorkspace":{"id":1}}],
            "workspaces":[{"id":1,"monitor":"DP-1","windows":0}], "windows":[]
        })));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let apply_focus = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let focus_effects = apply_focus.clone();
        let shared = state.clone();
        let calls = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut command = String::new();
                stream.read_to_string(&mut command).await.unwrap();
                let preflight = match command.as_str() {
                    "j/status" => Some(r#"{"configProvider":"lua"}"#),
                    "j/version" => Some(r#"{"version":"0.57.1"}"#),
                    _ if command.starts_with("eval assert(type(") => Some("ok"),
                    _ => None,
                };
                if let Some(reply) = preflight {
                    shared.lock().await["capabilities_requested"] = true.into();
                    let _ = stream.write_all(reply.as_bytes()).await;
                    continue;
                }
                calls.lock().await.push(command.clone());
                if focus_effects.load(std::sync::atomic::Ordering::Relaxed)
                    && command.starts_with("eval ")
                    && let Some((_, destination)) = command.split_once("hl.dsp.focus({workspace=")
                {
                    let id: i64 = destination.split('}').next().unwrap().parse().unwrap();
                    let mut state = shared.lock().await;
                    state["monitors"][0]["activeWorkspace"]["id"] = id.into();
                    if !state["workspaces"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|w| w["id"] == id)
                    {
                        state["workspaces"].as_array_mut().unwrap().push(json!({
                            "id":id,"monitor":"DP-1","windows":0
                        }));
                    }
                }
                if focus_effects.load(std::sync::atomic::Ordering::Relaxed)
                    && let Some((_, movement)) =
                        command.split_once("hl.dsp.window.move({window=w,workspace=")
                {
                    let id: i64 = movement.split(',').next().unwrap().parse().unwrap();
                    let address = command
                        .rsplit_once("hl.get_window('address:")
                        .map(|(_, tail)| tail.split('\'').next().unwrap());
                    let mut state = shared.lock().await;
                    for window in state["windows"].as_array_mut().unwrap() {
                        if address.is_some_and(|address| window["address"] == address) {
                            window["workspace"]["id"] = id.into();
                        }
                    }
                    state["pinned_status"] = "present".into();
                }
                let reply = if reference_cleanup(&command)
                    && shared.lock().await["reject_cleanup"] == true
                {
                    "cleanup rejected".into()
                } else if command.contains("shelllist-handoff-gone") {
                    match shared.lock().await["pinned_status"]
                        .as_str()
                        .unwrap_or("present")
                    {
                        "present" => "ok".into(),
                        status => format!("error: shelllist-handoff-{status}"),
                    }
                } else if command.starts_with("[[BATCH]]") {
                    let state = shared.lock().await;
                    format!(
                        "{}{}\n\n{}\n\n{}",
                        if command.starts_with("[[BATCH]]eval ") {
                            "ok\n\n"
                        } else {
                            ""
                        },
                        state["monitors"],
                        state["workspaces"],
                        state["windows"]
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
            apply_focus,
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
    async fn assert_read_only_requests(&self) {
        let requests = self.requests.lock().await;
        assert!(
            requests
                .iter()
                .all(|r| r.starts_with("[[BATCH]]") || reference_cleanup(r)),
            "{requests:?}"
        );
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
    async fn manual(&self, key: &str) -> Value {
        self.apply_focus
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.service
            .call(
                "workspace.command",
                json!({"command":"create","request_id":key}),
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn empty_icon_state_survives_restart_and_cues_use_bounded_first_mapped_identity() -> Result<()>
{
    let fixture = Fixture::new().await;
    fixture.manual("icon-manual").await;
    {
        let mut slot = fixture.service.loaded.lock().await;
        let loaded = slot.as_mut().context("initialized workspace")?;
        let entry = loaded.journal.entries.get_mut(&6).context("manual entry")?;
        entry.icon = "previous-app".into();
        entry.origin = "Previous".into();
        entry.cue_assigned = true;
        entry.cue_empty = false;
        entry.occupied = true;
        loaded.save().await?;
    }
    fixture.service.project(&mut Default::default()).await;
    let path = fixture.root.path().join("bar-daemon/workspaces-test.json");
    assert!(Journal::load(&path, "test")?.entries[&6].cue_empty);
    let restarted = Fixture::service(fixture.root.path());
    let maximum = "x".repeat(256);
    let oversized = "x".repeat(257);
    for (class, initial, origin) in [
        (
            "unmatched-workspace-test-app",
            "",
            "unmatched-workspace-test-app",
        ),
        ("", "workspace-test-initial", "workspace-test-initial"),
        ("", "", "Workspace"),
        (&maximum, "", &maximum),
        (&oversized, "", "Workspace"),
        ("bad\nclass", "", "Workspace"),
    ] {
        // Unmapped windows and larger addresses must not choose the cue.
        fixture.state.lock().await["windows"] = json!([
            {"address":"0x456","pid":42,"mapped":true,"workspace":{"id":6},"class":"Other"},
            {"address":"0x123","pid":42,"mapped":true,"workspace":{"id":6},"class":class,"initialClass":initial},
            {"address":"0x001","pid":42,"mapped":false,"workspace":{"id":6},"class":"Unmapped"}
        ]);
        for _ in 0..2 {
            restarted.project(&mut Default::default()).await;
            let journal = Journal::load(&path, "test")?;
            let entry = &journal.entries[&6];
            assert_eq!(entry.icon, "workspace");
            assert_eq!(entry.origin, origin);
            assert!(!entry.cue_empty && entry.cue_assigned && entry.occupied);
            fixture.state.lock().await["windows"][1]["class"] = "Changed while occupied".into();
        }
        fixture.state.lock().await["windows"] = json!([]);
        restarted.project(&mut Default::default()).await;
    }
    Ok(())
}

#[tokio::test]
async fn manual_workspace_survives_switching_away_becoming_empty_and_restart_until_explicit_close()
{
    let fixture = Fixture::new().await;
    let created = fixture.manual("manual").await;
    assert_eq!(created["status"], "focused");
    assert_eq!(created["lease"]["workspace_id"], 6);
    let generation = created["lease"]["generation"].clone();
    let mut projection = crate::model::WorkspaceState {
        available: true,
        monitors: vec![crate::model::MonitorState {
            id: 1,
            name: "DP-1".into(),
            focused: true,
            active_workspace_id: Some(1),
            active_workspace_address: None,
        }],
        ..Default::default()
    };
    {
        let mut state = fixture.state.lock().await;
        state["monitors"][0]["activeWorkspace"]["id"] = 1.into();
        // Hyprland reclaims the empty inactive workspace; the logical ID stays.
        state["workspaces"] = json!([{"id":1,"monitor":"DP-1","windows":0}]);
    }
    fixture.service.project(&mut projection).await;
    let restored = Fixture::service(fixture.root.path());
    restored.project(&mut projection).await;
    assert!(
        projection.lifecycle.monitors["DP-1"]
            .iter()
            .any(|item| item.id == 6)
    );
    {
        let guard = restored.loaded.lock().await;
        let entry = &guard.as_ref().unwrap().journal.entries[&6];
        assert!(entry.keep_until_closed);
        assert_eq!(serde_json::to_value(&entry.generation).unwrap(), generation);
    }
    // Retained destinations can still receive a new launch reservation.
    let mut request = Fixture::request("into-manual");
    request.workspace_id = Some(6);
    let reserved = restored
        .call("workspace.create", serde_json::to_value(request).unwrap())
        .await
        .unwrap();
    {
        let mut state = fixture.state.lock().await;
        state["workspaces"] = json!([{"id":6,"monitor":"DP-1","windows":1}]);
        state["windows"] = json!([{"address":"0x123","pid":42,"workspace":{"id":6},"mapped":true}]);
        state["monitors"][0]["activeWorkspace"]["id"] = 6.into();
    }
    restored.project(&mut projection).await;
    restored
        .call("workspace.lease.release", reserved["lease"].clone())
        .await
        .unwrap();
    fixture.state.lock().await["windows"] = json!([]);
    restored.project(&mut projection).await;
    assert!(
        restored
            .loaded
            .lock()
            .await
            .as_ref()
            .unwrap()
            .journal
            .entries[&6]
            .occupied
    );
    assert_eq!(
        fixture.state.lock().await["monitors"][0]["activeWorkspace"]["id"],
        6
    );
    restored
        .call(
            "workspace.command",
            json!({"command":"close","request_id":"close-manual"}),
        )
        .await
        .unwrap();
    for _ in 0..50 {
        if restored
            .loaded
            .lock()
            .await
            .as_ref()
            .unwrap()
            .journal
            .closes["close-manual"]
            .status
            == "closed"
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    restored.project(&mut projection).await;
    assert!(
        !restored
            .loaded
            .lock()
            .await
            .as_ref()
            .unwrap()
            .journal
            .entries
            .contains_key(&6)
    );
    assert_ne!(
        fixture.state.lock().await["monitors"][0]["activeWorkspace"]["id"],
        6
    );
    // Durable close admission must not be migrated back into a kept workspace.
    let restarted = Fixture::service(fixture.root.path());
    restarted.project(&mut projection).await;
    assert!(
        !restarted
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

#[tokio::test]
async fn pending_empty_close_cannot_readopt_a_manual_workspace() {
    let fixture = Fixture::new().await;
    fixture.manual("manual-close-race").await;
    {
        let mut guard = fixture.service.loaded.lock().await;
        let loaded = guard.as_mut().unwrap();
        let entry = loaded.journal.entries.get_mut(&6).unwrap();
        entry.keep_until_closed = false;
        entry.landing_pad = false;
        loaded.journal.closes.insert(
            "pending-close".into(),
            CloseRecord {
                request: wire::Close {
                    request_id: "pending-close".into(),
                    context: Context {
                        session: "test".into(),
                        monitor: "DP-1".into(),
                        workspace_id: 6,
                    },
                },
                targets: vec![],
                status: "waiting".into(),
                error: None,
            },
        );
        loaded.save().await.unwrap();
    }
    let mut projection = crate::model::WorkspaceState::default();
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
            .entries[&6]
            .landing_pad
    );
    // Restart fences the observer but must still complete empty retirement.
    let restarted = Fixture::service(fixture.root.path());
    restarted.project(&mut projection).await;
    assert!(
        !restarted
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

#[tokio::test]
async fn older_manual_journals_migrate_but_close_admission_stays_revoked() {
    let fixture = Fixture::new().await;
    fixture.manual("legacy-manual").await;
    let path = fixture
        .service
        .loaded
        .lock()
        .await
        .as_ref()
        .unwrap()
        .path
        .clone();
    let mut old: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    old["entries"]["6"]
        .as_object_mut()
        .unwrap()
        .remove("keep_until_closed");
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let restored = Journal::load(&path, "test").unwrap();
    assert!(restored.entries[&6].keep_until_closed);
    old["closes"]["legacy-close"] = json!({
        "request":{"request_id":"legacy-close","context":{"session":"test","monitor":"DP-1","workspace_id":6}},
        "targets":[],"status":"dispatching","error":null
    });
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let restored = Journal::load(&path, "test").unwrap();
    assert!(!restored.entries[&6].keep_until_closed);
    assert_eq!(restored.closes["legacy-close"].status, "unknown");
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
    fixture.assert_read_only_requests().await;
}

#[tokio::test]
async fn close_admission_fences_existing_launch_lease() {
    let fixture = Fixture::new().await;
    let mut request = Fixture::request("pending-launch");
    request.workspace_id = Some(1);
    let reserved = fixture
        .service
        .call("workspace.create", serde_json::to_value(request).unwrap())
        .await
        .unwrap();
    fixture.service.call("workspace.close", json!({"request_id":"close-pending", "context":{"session":"test","monitor":"DP-1","workspace_id":1}})).await.unwrap();
    let result = fixture.service.call("workspace.placement.apply", json!({"lease":reserved["lease"], "window":{"address":"0x123", "pid":42,"workspace_id":1}, "follow":true})).await;
    assert!(result.unwrap_err().to_string().contains("fenced"));
    assert!(
        !fixture
            .requests
            .lock()
            .await
            .iter()
            .any(|r| r.contains("window.move"))
    );
}

#[tokio::test]
async fn special_workspace_windows_are_excluded_from_ordinary_close_targets() {
    let fixture = Fixture::new().await;
    fixture.state.lock().await["windows"] = json!([
        {"address":"0x123","pid":42,"workspace":{"id":1},"mapped":true},
        {"address":"0x456","pid":43,"workspace":{"address":"special:probe","name":"special:probe","type":"special"},"mapped":true}
    ]);
    let admitted = fixture.service.call("workspace.close", json!({
        "request_id":"ordinary-close", "context":{"session":"test","monitor":"DP-1","workspace_id":1}
    })).await.unwrap();
    assert_eq!(admitted["targets"].as_array().unwrap().len(), 1);
    assert_eq!(admitted["targets"][0]["window"]["address"], "0x123");
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
    request.purpose = Purpose::LandingPad;
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
    fixture.assert_read_only_requests().await;
}

#[tokio::test]
async fn adopted_empty_workspace_survives_unknown_reads_and_output_migration() -> Result<()> {
    let fixture = Fixture::new().await;
    let reserved = fixture.create("adopt").await;
    {
        let mut state = fixture.state.lock().await;
        state["monitors"][0]["activeWorkspace"]["id"] = 6.into();
        state["workspaces"] = json!([{"id":6,"monitor":"DP-1","windows":0}]);
    }
    let mut projection = crate::model::WorkspaceState::default();
    fixture.service.project(&mut projection).await;
    fixture
        .service
        .call("workspace.lease.release", reserved["lease"].clone())
        .await?;
    let path = fixture.root.path().join("bar-daemon/workspaces-test.json");
    let entry = || -> Result<Value> {
        Ok(serde_json::to_value(
            &Journal::load(&path, "test")?.entries[&6],
        )?)
    };
    let original = entry()?;
    assert_eq!(original["landing_pad"], true);
    fixture.state.lock().await["windows"] = json!("unavailable");
    fixture.service.project(&mut projection).await;
    assert!(!projection.lifecycle.available);
    assert_eq!(
        entry()?,
        original,
        "unknown reads cannot change the durable entry"
    );
    {
        let mut state = fixture.state.lock().await;
        state["windows"] = json!([]);
        state["monitors"][0]["name"] = "DP-2".into();
        state["workspaces"][0]["monitor"] = "DP-2".into();
    }
    let mut migrated = original;
    migrated["monitor"] = "DP-2".into();
    for _ in 0..2 {
        fixture.service.project(&mut projection).await;
        assert!(projection.lifecycle.error.is_none());
        assert_eq!(
            entry()?,
            migrated,
            "only ownership changes, including repeated observations"
        );
    }
    fixture.state.lock().await["monitors"][0]["activeWorkspace"]["id"] = 1.into();
    fixture.service.project(&mut projection).await;
    assert!(!Journal::load(&path, "test")?.entries.contains_key(&6));
    fixture.assert_read_only_requests().await;
    Ok(())
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
    tokio::time::sleep(handoff::GRACE).await;
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

impl Fixture {
    async fn tracked_launch(&self) -> Value {
        let reserved = self.create("tracked").await;
        self.apply_focus
            .store(true, std::sync::atomic::Ordering::Relaxed);
        {
            let mut state = self.state.lock().await;
            state["windows"] = json!([{"address":"0x123","pid":std::process::id(),"workspace":{"id":1},"mapped":true}]);
            state["workspaces"] = json!([{"id":1,"monitor":"DP-1","windows":0},{"id":6,"monitor":"DP-1","windows":1}]);
        }
        self.service.call("workspace.placement.apply", json!({"lease":reserved["lease"],"window":{"address":"0x123","pid":std::process::id(),"workspace_id":1},"follow":false})).await.unwrap();
        self.service
            .call("workspace.lease.release", reserved["lease"].clone())
            .await
            .unwrap();
        reserved
    }
    async fn replacement(&self, pid: u32) {
        let mut state = self.state.lock().await;
        state["pinned_status"] = "gone".into();
        // Recycled address, different compositor object.
        state["windows"] =
            json!([{"address":"0x123","pid":pid,"workspace":{"id":1},"mapped":true}]);
    }
}

#[tokio::test]
async fn replacement_reuses_address_but_preserves_destination_without_following() {
    let fixture = Fixture::new().await;
    fixture.tracked_launch().await;
    fixture.replacement(std::process::id()).await;
    fixture
        .service
        .project(&mut crate::model::WorkspaceState::default())
        .await;
    let state = fixture.state.lock().await;
    assert_eq!(state["windows"][0]["workspace"]["id"], 6);
    assert_eq!(state["monitors"][0]["activeWorkspace"]["id"], 1);
    let loaded = fixture.service.loaded.lock().await;
    assert_eq!(
        loaded.as_ref().unwrap().journal.handoffs["tracked"].replacements,
        1
    );
}

#[tokio::test]
async fn empty_gap_is_retained_then_expires_without_further_window_events() {
    let fixture = Fixture::new().await;
    fixture.tracked_launch().await;
    {
        let mut state = fixture.state.lock().await;
        state["windows"] = json!([]);
        state["pinned_status"] = "gone".into();
    }
    fixture
        .service
        .project(&mut crate::model::WorkspaceState::default())
        .await;
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
    tokio::time::timeout(
        handoff::GRACE + Duration::from_secs(1),
        fixture.service.changed.notified(),
    )
    .await
    .unwrap();
    fixture
        .service
        .project(&mut crate::model::WorkspaceState::default())
        .await;
    let loaded = fixture.service.loaded.lock().await;
    assert!(!loaded.as_ref().unwrap().journal.entries.contains_key(&6));
    assert!(loaded.as_ref().unwrap().journal.handoffs.is_empty());
}

#[tokio::test]
async fn manual_move_even_followed_by_close_or_return_cancels_replacement() {
    for moved in [
        "movewindowv2>>123,3,3",
        "movewindowv2>>123,special:probe,special:probe",
        "movewindow>>123,project",
        "movewindowv2>>123,project,project",
    ] {
        let fixture = Fixture::new().await;
        fixture.tracked_launch().await;
        fixture
            .service
            .handoff_event(&shelllist_hyprland::Event::Compositor(
                shelllist_hyprland::CompositorEvent::parse(moved),
            ));
        fixture
            .service
            .handoff_event(&shelllist_hyprland::Event::Compositor(
                shelllist_hyprland::CompositorEvent::parse("movewindowv2>>123,6,6"),
            ));
        fixture.replacement(std::process::id()).await;
        fixture
            .service
            .project(&mut crate::model::WorkspaceState::default())
            .await;
        assert_eq!(
            fixture.state.lock().await["windows"][0]["workspace"]["id"],
            1
        );
        assert!(
            fixture
                .service
                .loaded
                .lock()
                .await
                .as_ref()
                .unwrap()
                .journal
                .handoffs
                .is_empty()
        );
    }
}

#[tokio::test]
async fn restart_explicit_close_and_lost_identity_never_replay_replacement() {
    for reason in ["restart", "close", "lost", "disconnect"] {
        let fixture = Fixture::new().await;
        fixture.tracked_launch().await;
        if reason == "close" {
            fixture.state.lock().await["monitors"][0]["activeWorkspace"]["id"] = 6.into();
            fixture
                .service
                .call(
                    "workspace.command",
                    json!({"command":"close","request_id":"close-tracked"}),
                )
                .await
                .unwrap();
        }
        fixture.replacement(std::process::id()).await;
        if reason == "lost" {
            fixture.state.lock().await["pinned_status"] = "lost".into();
        }
        if reason == "disconnect" {
            fixture
                .service
                .handoff_event(&shelllist_hyprland::Event::Disconnected(None));
        }
        let service = if reason == "restart" {
            Fixture::service(fixture.root.path())
        } else {
            fixture.service.clone()
        };
        service
            .project(&mut crate::model::WorkspaceState::default())
            .await;
        assert_eq!(
            fixture.state.lock().await["windows"][0]["workspace"]["id"],
            1,
            "{reason}"
        );
        assert!(
            service
                .loaded
                .lock()
                .await
                .as_ref()
                .unwrap()
                .journal
                .handoffs
                .is_empty(),
            "{reason}"
        );
    }
}

#[tokio::test]
async fn unrelated_process_and_reused_pid_are_not_replacement_authority() {
    let fixture = Fixture::new().await;
    fixture.tracked_launch().await;
    fixture.replacement(std::process::id() + 1).await;
    fixture
        .service
        .project(&mut crate::model::WorkspaceState::default())
        .await;
    assert_eq!(
        fixture.state.lock().await["windows"][0]["workspace"]["id"],
        1
    );
    fixture.replacement(std::process::id()).await;
    fixture
        .service
        .loaded
        .lock()
        .await
        .as_mut()
        .unwrap()
        .journal
        .handoffs
        .get_mut("tracked")
        .unwrap()
        .process_start += 1;
    fixture
        .service
        .project(&mut crate::model::WorkspaceState::default())
        .await;
    assert_eq!(
        fixture.state.lock().await["windows"][0]["workspace"]["id"],
        1
    );
    assert!(
        fixture
            .service
            .loaded
            .lock()
            .await
            .as_ref()
            .unwrap()
            .journal
            .handoffs
            .is_empty()
    );
}
