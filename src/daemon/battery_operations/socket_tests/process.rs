//! OS-process crash qualification of private runtime components, not daemon startup.
//! The child is this lib-test executable, never the production monitoring entrypoint.
use super::*;
mod payload;
use crate::{
    activity::{
        ActivityService,
        notifications::{engine::NotificationEngine, service::NotificationService},
    },
    api::ApiService,
    daemon::subscriptions::BarDaemon,
    media::MediaService,
    state::StateStore,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::Write,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
};
use tokio::process::{Child, Command};

const DEADLINE: Duration = Duration::from_secs(5);
const CHILD_TEST: &str = "daemon::battery_operations::socket_tests::process::private_runtime_child";

struct Runtime {
    daemon: BarDaemon,
    operations: Operations,
    state: StateStore,
    settled: Arc<Semaphore>,
    audit: PathBuf,
}

fn audit(path: &Path, event: &str, case: &str) {
    // Synthetic labels only, not operation IDs, contents or real outcome evidence.
    writeln!(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap(),
        "{event}:{case}"
    )
    .unwrap();
}

impl LocalJsonlService for Runtime {
    async fn call(&self, session: &SocketSession, method: &str, params: Value) -> Result<Value> {
        match method {
            "fixture.battery.start" => {
                let case = params["case"].as_str().context("fixture case")?;
                ensure!(
                    [
                        "released",
                        "before-receipt",
                        "accepted",
                        "cancelling",
                        "terminal",
                        "fresh"
                    ]
                    .contains(&case),
                    "unknown fixture case"
                );
                let case = case.to_owned();
                let path = self.audit.clone();
                let settled = self.settled.clone();
                // The real battery admission/cancellation/retention coordinator;
                // replace only the authorization/effect future, before any helper.
                let response = self
                    .operations
                    .start(Owner::Socket(session.clone()), async move {
                        let mut cancellation = authorization::cancellation().unwrap();
                        audit(&path, "started", &case);
                        tokio::select! {
                            () = authorization::cancelled(&mut cancellation) => {
                                audit(&path, "cancelled", &case);
                                settled.acquire().await.unwrap().forget();
                            }
                            permit = settled.acquire() => { permit.unwrap().forget(); }
                        }
                        audit(&path, "settled", &case);
                        if case == "terminal" {
                            api::success(json!({"fixture_completed":true}))
                        } else {
                            api::error(
                                "outcome-unknown",
                                "fake work reconciled; cancellation is not rollback",
                            )
                        }
                    });
                if params["withhold_receipt"] == true && response["ok"] == true {
                    // Accepted work survives the reply waiter; the parent kills
                    // this process without ever receiving its operation ID.
                    std::future::pending::<()>().await;
                }
                Ok(response)
            }
            "battery.operation.status" | "battery.operation.cancel" => {
                Ok(self.operations.lookup(method, params, session.owner()))
            }
            "fixture.reconcile" => {
                self.settled.add_permits(1);
                Ok(api::success(json!({})))
            }
            "fixture.counts" => {
                let store = self.operations.0.lock().unwrap();
                Ok(api::success(json!({
                    "active": store.entries.values().filter(|e| e.response.is_none()).count(),
                    "finished": store.entries.values().filter(|e| e.response.is_some()).count(),
                    "demand": *self.state.work_area_demand().borrow(),
                })))
            }
            "fixture.publish" => {
                self.state.record_sleep_preparation(true).await;
                Ok(api::success(json!({})))
            }
            "fixture.payload" => {
                let bytes = params["bytes"].as_u64().context("payload bytes")?;
                ensure!(bytes <= 32 * 1024 * 1024, "fixture payload bound");
                self.state
                    .update_battery(crate::model::BatteryState {
                        error: Some("x".repeat(bytes as usize)),
                        ..Default::default()
                    })
                    .await;
                Ok(api::success(json!({})))
            }
            "bar.snapshot" => Ok(serde_json::from_str(
                &self
                    .daemon
                    .call_owned(
                        method,
                        &params.to_string(),
                        Some(Owner::Socket(session.clone())),
                    )
                    .await,
            )?),
            // Allowlist BEFORE dispatch: no hardware, notifications, display
            // effects, application launch, host services or battery helpers.
            _ => bail!("private runtime refuses method {method}"),
        }
    }
    async fn subscribe(&self, session: &SocketSession, streams: Vec<String>) -> Result<Value> {
        ensure!(
            streams.iter().all(
                |s| ["battery.changed", "power-sleep.changed", "workarea.changed"]
                    .contains(&s.as_str())
            ),
            "unexpected fixture stream"
        );
        Ok(serde_json::from_str(
            &self
                .daemon
                .subscribe_owned(
                    streams,
                    Some(Owner::Socket(session.clone())),
                    session.clone().into(),
                )
                .await,
        )?)
    }
    async fn cancel(&self, session: &SocketSession, id: &str) -> Result<Value> {
        Ok(serde_json::from_str(
            &self.daemon.cancel_owned(id, Some(session.owner())).await,
        )?)
    }
    async fn disconnected(&self, _: &SocketSession) {}
}

#[tokio::test]
#[ignore = "private child entrypoint; invoked only by the process-crash parent test"]
async fn private_runtime_child() -> Result<()> {
    // --include-ignored alone must not start a listener or a process that waits.
    let Some(root) = std::env::var_os("BAR_PRIVATE_RUNTIME_CHILD") else {
        return Ok(());
    };
    let root = std::fs::canonicalize(root)?;
    ensure!(
        std::env::var_os("XDG_RUNTIME_DIR") == Some(root.join("run").into_os_string()),
        "private runtime mismatch"
    );
    let state = StateStore::default();
    let activity = ActivityService::with_paths(
        state.clone(),
        root.join("config/activity.json"),
        root.join("data/todos.json"),
    )
    .await;
    let api = ApiService::new(
        state.clone(),
        activity,
        NotificationService::native(NotificationEngine::new(state.clone()).await),
        MediaService::default(),
        crate::brightness::BrightnessService::new(state.clone()),
    );
    let runtime = Arc::new(Runtime {
        daemon: BarDaemon::new(api, state.clone()),
        operations: Operations::default(),
        state,
        settled: Arc::new(Semaphore::new(0)),
        audit: root.join("audit"),
    });
    let listener = JsonlListener::bind(&root.join("run"), "bar-daemon").await?;
    // Also bound the child if a broken parent never kills it.
    let mut config = JsonlSocketConfig::default();
    if root.join("short-write-deadline").exists() {
        config.write_timeout = Duration::from_millis(100);
    }
    timeout(
        Duration::from_secs(45),
        listener.serve(runtime, BasicCorrelation, config, std::future::pending()),
    )
    .await??;
    Ok(())
}

struct Process {
    child: Child,
    root: PathBuf,
}
impl Process {
    fn spawn(root: &Path, generation: usize) -> Result<Self> {
        let log = std::fs::File::create(root.join(format!("child-{generation}.log")))?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
            .env_clear()
            .env("BAR_PRIVATE_RUNTIME_CHILD", root)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_DATA_DIRS", root.join("data"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("TMPDIR", root.join("tmp"))
            .env("BAR_DAEMON_BACKLIGHT_ROOT", root.join("backlight"))
            .env(
                "DBUS_SYSTEM_BUS_ADDRESS",
                "unix:path=/nonexistent-bar-runtime-system",
            )
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent-bar-runtime-session",
            )
            .env("WAYLAND_DISPLAY", "nonexistent-bar-runtime-wayland")
            .env(
                "HYPRLAND_INSTANCE_SIGNATURE",
                "nonexistent-bar-runtime-hyprland",
            )
            .env("TOKIO_WORKER_THREADS", "2")
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true);
        Ok(Self {
            child: command.spawn()?,
            root: root.into(),
        })
    }
    async fn peer(&mut self) -> Result<Peer> {
        timeout(DEADLINE, async {
            loop {
                if let Some(status) = self.child.try_wait()? {
                    bail!(
                        "private child exited {status}; inspect {}",
                        self.root.display()
                    );
                }
                match UnixStream::connect(self.root.join("run/bar-daemon.sock")).await {
                    Ok(io) => {
                        return Ok(Peer {
                            io: BufReader::new(io),
                            events: Vec::new(),
                            next: 0,
                        });
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
        })
        .await?
    }
    async fn kill(&mut self) -> Result<()> {
        self.child.start_kill()?;
        let status = timeout(DEADLINE, self.child.wait()).await??;
        ensure!(status.signal() == Some(9), "expected SIGKILL, got {status}");
        Ok(())
    }
}

struct Peer {
    io: BufReader<UnixStream>,
    events: Vec<Value>,
    next: usize,
}
impl Peer {
    async fn send(&mut self, mut value: Value) -> Result<Value> {
        self.next += 1;
        value["id"] = json!(format!("request-{}", self.next));
        let kind = match value["op"].as_str() {
            Some("subscribe") => "subscription",
            Some("call") => "call",
            _ => "control",
        };
        value["route"] =
            json!({"consumerId":"runtime", "localId":"read", "kind":kind, "generation":1});
        timeout(
            DEADLINE,
            self.io.get_mut().write_all(format!("{value}\n").as_bytes()),
        )
        .await??;
        Ok(value)
    }
    async fn record(&mut self) -> Result<Value> {
        let mut line = String::new();
        ensure!(
            timeout(DEADLINE, self.io.read_line(&mut line)).await?? > 0,
            "socket EOF"
        );
        Ok(serde_json::from_str(&line)?)
    }
    async fn request(&mut self, value: Value) -> Result<Value> {
        let request = self.send(value).await?;
        loop {
            let record = self.record().await?;
            if record["kind"] == "response" && record["id"] == request["id"] {
                ensure!(record["route"] == request["route"], "route changed");
                return Ok(record["response"].clone());
            }
            ensure!(
                record["kind"] == "event" && self.events.len() < 32,
                "unexpected record/backlog"
            );
            self.events.push(record);
        }
    }
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request(json!({"op":"call", "method":method, "params":params}))
            .await
    }
    /// Consume an acceptance receipt, never infer an ID from observed state.
    /// The lost-before-receipt case intentionally uses send() instead.
    async fn start_battery(&mut self, case: &str) -> Result<String> {
        let receipt = self
            .call("fixture.battery.start", json!({"case":case}))
            .await?;
        let operation = &receipt["data"]["operation"];
        ensure!(
            receipt["ok"] == true && operation["state"] == "running",
            "expected an admitted running battery receipt"
        );
        Ok(operation["id"]
            .as_str()
            .context("accepted operation ID")?
            .to_owned())
    }
    async fn subscribe(&mut self) -> Result<String> {
        let response = self.request(json!({"op":"subscribe", "streams":["battery.changed", "power-sleep.changed", "workarea.changed"]})).await?;
        Ok(response["data"]["subscription"]["id"]
            .as_str()
            .context("subscription ID")?
            .into())
    }
    async fn baseline(&mut self) -> Result<()> {
        while self.events.len() < 3 {
            let record = self.record().await?;
            self.events.push(record);
        }
        ensure!(
            self.events.len() == 3
                && self
                    .events
                    .iter()
                    .all(|e| e["kind"] == "event" && e["event"]["event"] == "subscribed"),
            "recovery must be baseline, not historical replay"
        );
        self.events.clear();
        Ok(())
    }
    async fn disconnected(&mut self) -> Result<()> {
        loop {
            let mut line = String::new();
            match timeout(DEADLINE, self.io.read_line(&mut line)).await? {
                Ok(0) => return Ok(()),
                Err(e)
                    if [
                        std::io::ErrorKind::ConnectionReset,
                        std::io::ErrorKind::BrokenPipe,
                    ]
                    .contains(&e.kind()) =>
                {
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
                Ok(_) => {} // drain only bounded pre-crash fixture records
            }
            ensure!(self.next < 64, "unbounded post-crash records");
            self.next += 1;
        }
    }
}
async fn counts(peer: &mut Peer, active: u64, finished: u64, demand: u64) -> Result<()> {
    timeout(DEADLINE, async {
        loop {
            let value = peer.call("fixture.counts", json!({})).await?;
            if value["data"] == json!({"active":active,"finished":finished,"demand":demand}) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}
async fn marker(root: &Path, label: &str) -> Result<()> {
    timeout(DEADLINE, async {
        loop {
            if std::fs::read_to_string(root.join("audit"))
                .unwrap_or_default()
                .lines()
                .any(|line| line == label)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}
fn root() -> Result<tempfile::TempDir> {
    let root = tempfile::tempdir()?;
    for name in [
        "home",
        "config",
        "data",
        "cache",
        "state",
        "run",
        "tmp",
        "backlight",
    ] {
        std::fs::create_dir(root.path().join(name))?;
        std::fs::set_permissions(
            root.path().join(name),
            std::fs::Permissions::from_mode(0o700),
        )?;
    }
    Ok(root)
}
async fn snapshot(peer: &mut Peer) -> Result<Value> {
    let response = peer
        .call(
            "bar.snapshot",
            json!({"streams":["battery.changed", "power-sleep.changed"]}),
        )
        .await?;
    ensure!(response["ok"] == true, "cached read failed");
    Ok(response["data"]["snapshot"].clone())
}

#[tokio::test]
async fn process_crash_loses_private_outcomes_without_replay_or_peer_authority() -> Result<()> {
    timeout(Duration::from_secs(90), async {
        for case in ["before-receipt", "accepted", "cancelling", "terminal"] {
            let directory = root()?;
            let root = std::fs::canonicalize(directory.path())?;
            let mut process = Process::spawn(&root, 1)?;
            let mut a = process.peer().await?;
            let mut b = process.peer().await?;
            let old_subscription = a.subscribe().await?;
            b.subscribe().await?;
            a.baseline().await?;
            b.baseline().await?;
            counts(&mut b, 0, 0, 2).await?;
            let old_snapshot = snapshot(&mut b).await?;

            // Graceful owner loss is cancellation, not admission release or rollback.
            let old_id = a.start_battery("released").await?;
            marker(&root, "started:released").await?;
            for method in ["battery.operation.status", "battery.operation.cancel"] {
                ensure!(b.call(method, json!({"operation_id":old_id})).await?["error"]["code"] == "operation-not-found", "foreign battery authority");
            }
            ensure!(b.request(json!({"op":"cancel", "request_id":old_subscription})).await?["error"]["code"] == "request-not-found", "foreign subscription authority");
            drop(a);
            marker(&root, "cancelled:released").await?;
            counts(&mut b, 1, 0, 1).await?;
            ensure!(b.call("fixture.battery.start", json!({"case":case})).await?["error"]["code"] == "operation-active", "cancelled work lost admission before reconciliation");
            snapshot(&mut b).await?; // healthy peer, while cancellation is pending
            b.call("fixture.reconcile", json!({})).await?;
            counts(&mut b, 0, 0, 1).await?;

            let mut a = process.peer().await?;
            let uncertain = if case == "before-receipt" {
                a.send(json!({"op":"call", "method":"fixture.battery.start", "params":{"case":case,"withhold_receipt":true}})).await?;
                None
            } else {
                Some(a.start_battery(case).await?)
            };
            marker(&root, &format!("started:{case}")).await?;
            counts(&mut b, 1, 0, 1).await?;
            if case == "cancelling" {
                let id = uncertain.as_ref().unwrap();
                let cancel = a.call("battery.operation.cancel", json!({"operation_id":id})).await?;
                ensure!(cancel["data"]["operation"]["state"] == "running" && cancel["data"]["operation"]["cancellation_requested"] == true, "cancellation claimed completion");
                marker(&root, "cancelled:cancelling").await?;
                counts(&mut b, 1, 0, 1).await?;
            }
            if case == "terminal" {
                b.call("fixture.reconcile", json!({})).await?;
                counts(&mut b, 0, 1, 1).await?; // terminal exists, owner has NOT read it
            }
            b.call("fixture.publish", json!({})).await?;
            ensure!(snapshot(&mut b).await?["power_sleep"]["preparing_for_sleep"] == true, "synthetic sleep edge missing");
            let audit_before = std::fs::read_to_string(root.join("audit"))?;
            ensure!(audit_before.lines().filter(|line| line.starts_with("started:")).count() == 2, "rejected work executed");

            // Actual private OS process SIGKILL, not listener recreation.
            process.kill().await?;
            a.disconnected().await?;
            b.disconnected().await?;
            ensure!(root.join("run/bar-daemon.sock").exists(), "kill unexpectedly performed graceful unlink");
            let mut restarted = Process::spawn(&root, 2)?;
            let mut fresh = restarted.peer().await?;
            let mut peer = restarted.peer().await?;
            fresh.subscribe().await?;
            peer.subscribe().await?;
            fresh.baseline().await?;
            peer.baseline().await?;
            counts(&mut peer, 0, 0, 2).await?;
            let current = snapshot(&mut fresh).await?;
            for domain in ["battery", "power_sleep"] {
                ensure!(current[domain]["_publication"]["epoch"].is_string() && current[domain]["_publication"]["epoch"] != old_snapshot[domain]["_publication"]["epoch"], "process restart reused publication epoch");
            }
            ensure!(current["power_sleep"]["preparing_for_sleep"] == false, "lost historical edge replayed");
            for id in std::iter::once(&old_id).chain(uncertain.iter()) {
                for method in ["battery.operation.status", "battery.operation.cancel"] {
                    ensure!(fresh.call(method, json!({"operation_id":id})).await?["error"]["code"] == "operation-not-found", "missing non-durable outcome inferred as success");
                }
            }
            // Recovery reads/status/subscriptions cannot start or settle old effects.
            snapshot(&mut peer).await?;
            ensure!(std::fs::read_to_string(root.join("audit"))? == audit_before, "recovery replayed or reconciled an uncertain effect");
            let next_id = fresh.start_battery("fresh").await?;
            ensure!(next_id != old_id && uncertain.as_deref() != Some(next_id.as_str()), "fresh owner reused a retired ID");
            marker(&root, "started:fresh").await?;
            drop(fresh);
            marker(&root, "cancelled:fresh").await?;
            counts(&mut peer, 1, 0, 1).await?;
            peer.call("fixture.reconcile", json!({})).await?;
            counts(&mut peer, 0, 0, 1).await?;
            snapshot(&mut peer).await?;
            let mut inspector = restarted.peer().await?;
            drop(peer);
            counts(&mut inspector, 0, 0, 0).await?;
            drop(inspector);
            restarted.kill().await?;
            println!("BAR_RUNTIME_PROCESS_PASS case={case}");
        }
        Ok::<_, anyhow::Error>(())
    }).await?
}
