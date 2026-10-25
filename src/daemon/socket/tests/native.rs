//! Explicit feature-gated native acceptance, not an ignored test silently included
//! by the release package. No monitors, hardware helpers or host buses are started.
use super::*;
use anyhow::{Context, ensure};
use std::{collections::HashSet, os::unix::fs::PermissionsExt, sync::Mutex};

#[derive(Default)]
struct Audit {
    owners: HashSet<String>,
    reads: usize,
    subscriptions: usize,
    cancellations: usize,
    disconnects: usize,
}
struct ReadOnlyProbe {
    service: Service,
    state: StateStore,
    audit: Arc<Mutex<Audit>>,
}
impl LocalJsonlService for ReadOnlyProbe {
    async fn call(&self, session: &SocketSession, method: &str, params: Value) -> Result<Value> {
        // Fail before dispatch if the probe ever attempts an effect.
        ensure!(
            method == "bar.snapshot",
            "probe attempted non-read method {method}"
        );
        let read = {
            let mut audit = self.audit.lock().unwrap();
            audit.owners.insert(session.owner().into());
            audit.reads += 1;
            audit.reads
        };
        if read == 8 {
            demand(&self.state, 2).await;
        }
        if read == 9 {
            demand(&self.state, 1).await;
        }
        let response = self.service.call(session, method, params).await?;
        if read == 8 {
            // Race a genuine new publication against the already captured read.
            // Either delivery order must converge to 43, not the captured 42.
            self.state
                .update_audio(crate::model::AudioState {
                    available: true,
                    volume_percent: 43,
                    ..Default::default()
                })
                .await;
        }
        Ok(response)
    }
    async fn subscribe(&self, session: &SocketSession, streams: Vec<String>) -> Result<Value> {
        ensure!(
            streams
                .iter()
                .all(|s| ["audio.changed", "battery.changed", "workarea.changed"]
                    .contains(&s.as_str())),
            "unexpected probe streams"
        );
        {
            let mut audit = self.audit.lock().unwrap();
            audit.owners.insert(session.owner().into());
            audit.subscriptions += 1;
        }
        self.service.subscribe(session, streams).await
    }
    async fn cancel(&self, session: &SocketSession, id: &str) -> Result<Value> {
        {
            let mut audit = self.audit.lock().unwrap();
            audit.owners.insert(session.owner().into());
            audit.cancellations += 1;
        }
        self.service.cancel(session, id).await
    }
    async fn disconnected(&self, session: &SocketSession) {
        self.audit.lock().unwrap().disconnects += 1;
        self.service.disconnected(session).await;
    }
}

#[tokio::test]
async fn native_quickshell_bar_socket_acceptance() -> Result<()> {
    let source = std::path::PathBuf::from(
        std::env::var_os("SHELLLIST_SOURCE")
            .context("SHELLLIST_SOURCE required for explicit native probe")?,
    );
    let program = std::env::var_os("SHELLLIST_QUICKSHELL")
        .context("SHELLLIST_QUICKSHELL required for explicit native probe")?;
    let directory = tempfile::tempdir()?;
    let runtime = directory.path().join("run");
    std::fs::create_dir(&runtime)?;
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
    let runtime = std::fs::canonicalize(runtime)?;
    let state = StateStore::default();
    state
        .update_audio(crate::model::AudioState {
            available: true,
            volume_percent: 42,
            ..Default::default()
        })
        .await;
    state
        .update_battery(crate::model::BatteryState {
            available: true,
            percentage: 71,
            ..Default::default()
        })
        .await;
    let daemon = fixture_daemon(directory.path(), state.clone()).await;
    let subscriptions = daemon.subscriptions.clone();
    let bus = crate::battery::helper::authorization_tests::Bus::new();
    let server = bus.connection().await;
    server.request_name(crate::api::BUS_NAME).await?;
    server.object_server().at(OBJECT_PATH, daemon).await?;
    let listener = JsonlListener::bind(&runtime, "bar-daemon").await?;
    let audit = Arc::new(Mutex::new(Audit::default()));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(listener.serve(
        Arc::new(ReadOnlyProbe {
            service: Service(server),
            state: state.clone(),
            audit: audit.clone(),
        }),
        BasicCorrelation,
        JsonlSocketConfig::default(),
        async {
            let _ = stopped.await;
        },
    ));
    let child = tokio::process::Command::new(program)
        .args(["--no-color", "--path"])
        .arg(source.join("tests/e2e/bar-unix-jsonl.qml"))
        .env_clear()
        .env("HOME", directory.path())
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("QML_IMPORT_PATH", source.join("qml"))
        .env("QT_QPA_PLATFORM", "offscreen")
        .env("QT_QUICK_BACKEND", "software")
        .env(
            "DBUS_SYSTEM_BUS_ADDRESS",
            "unix:path=/nonexistent-system-bus",
        )
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent-session-bus",
        )
        .env("SHELLLIST_BAR_SOCKET", "1")
        .env("SHELLLIST_PRIVATE_BAR_PROBE", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let result = timeout(Duration::from_secs(25), child.wait_with_output()).await;
    // Verify owner cleanup before stopping the listener, not as a shutdown side effect.
    demand(&state, 0).await;
    let _ = stop.send(());
    timeout(DEADLINE, serving).await???;
    subscriptions.shutdown().await;
    let output = result??;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        output.status.success() && log.contains("BAR_SOCKET_PROBE_PASS"),
        "{log}"
    );
    let audit = audit.lock().unwrap();
    ensure!(audit.owners.len() == 1, "consumers did not share one owner");
    ensure!(
        audit.reads == 9 && audit.subscriptions == 3,
        "unexpected reads/subscriptions: {}/{}",
        audit.reads,
        audit.subscriptions
    );
    ensure!(
        audit.cancellations >= 2 && audit.disconnects == 1,
        "missing cancellation/disconnect cleanup"
    );
    println!("{log}");
    Ok(())
}
