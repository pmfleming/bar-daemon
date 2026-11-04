//! Real Qt replacement windows on an isolated compositor and session bus.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use shelllist_hyprland::{Client, workspace};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command},
    time::Duration,
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(
    binary: &Path,
    args: &[&str],
    root: &Path,
    display: &str,
    signature: &str,
    log: &str,
) -> Result<Process> {
    let file = fs::File::create(root.join(log))?;
    Ok(Process(
        Command::new(binary)
            .args(args)
            .env_clear()
            .env("HOME", root)
            .env("XDG_RUNTIME_DIR", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("PATH", root.join("bin"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/bus", root.display()),
            )
            .env(
                "DBUS_SYSTEM_BUS_ADDRESS",
                format!("unix:path={}/no-system-bus", root.display()),
            )
            .env("WAYLAND_DISPLAY", display)
            .env("HYPRLAND_INSTANCE_SIGNATURE", signature)
            .env("HYPRLAND_NO_SD_NOTIFY", "1")
            .env("HYPRLAND_NO_SD_VARS", "1")
            .env("AQ_DRM_DEVICES", "/dev/null")
            .env("LIBSEAT_BACKEND", "noop")
            .env("QT_QPA_PLATFORM", "wayland")
            .env("QT_QUICK_BACKEND", "software")
            .env("QML_XHR_ALLOW_FILE_READ", "1")
            .env("QT_LOGGING_RULES", "qml.debug=true;qml.warning=true")
            .env(
                "QT_PLUGIN_PATH",
                std::env::var("WORKSPACE_TEST_QT_PLUGINS")?,
            )
            .stdout(file.try_clone()?)
            .stderr(file)
            .spawn()?,
    ))
}

async fn call(bus: &zbus::Connection, method: &str, params: Value) -> Result<Value> {
    let proxy = zbus::Proxy::new(
        bus,
        "org.laufan.BarDaemon",
        "/org/laufan/BarDaemon",
        "org.laufan.BarDaemon2",
    )
    .await?;
    let reply: String = tokio::time::timeout(
        Duration::from_secs(10),
        proxy.call("Call", &(method, params.to_string())),
    )
    .await??;
    let value: Value = serde_json::from_str(&reply)?;
    ensure!(value["ok"] == true, "{method}: {value}");
    Ok(value["data"].clone())
}

async fn window(client: &Client, pid: u32, workspace: Option<i64>) -> Result<workspace::Window> {
    for _ in 0..120 {
        if let Some(w) = workspace::snapshot(client)
            .await?
            .windows
            .into_iter()
            .find(|w| w.mapped && w.pid == pid && workspace.is_none_or(|id| w.workspace.id == id))
        {
            return Ok(w);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("Qt window did not appear on {workspace:?}")
}

#[tokio::test]
#[ignore = "requires WORKSPACE_TEST_HYPRLAND, PARENT_DISPLAY, DBUS, QML, QT_PLUGINS"]
async fn qt_login_replacement_retains_workspace_and_respects_cancellation() -> Result<()> {
    let root = tempfile::Builder::new()
        .prefix("handoff-")
        .tempdir_in("/tmp")?
        .keep();
    eprintln!("private Qt handoff evidence: {}", root.display());
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    for name in ["config", "cache", "data", "bin"] {
        fs::create_dir_all(root.join(name))?;
    }
    let config = root.join("config/hyprland.lua");
    fs::write(
        &config,
        "-- Isolated handoff test; no user configuration.\n",
    )?;
    let _compositor = spawn(
        Path::new(&std::env::var("WORKSPACE_TEST_HYPRLAND")?),
        &["--config", config.to_str().unwrap()],
        &root,
        &std::env::var("WORKSPACE_TEST_PARENT_DISPLAY")?,
        "",
        "hyprland.log",
    )?;
    let mut transport = None;
    for _ in 0..150 {
        if let Ok(entries) = fs::read_dir(root.join("hypr")) {
            for entry in entries.flatten() {
                let client = Client::new(
                    root.clone(),
                    Some(entry.file_name().to_string_lossy().into_owned()),
                );
                if workspace::snapshot(&client).await.is_ok() {
                    transport = Some(client);
                    break;
                }
            }
        }
        if transport.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let client = transport.context("private compositor did not start")?;
    let signature = workspace::snapshot(&client).await?.session;
    let display = fs::read_dir(&root)?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("wayland-") && !name.ends_with(".lock"))
        .context("private display")?;
    let address = format!("unix:path={}/bus", root.display());
    let _dbus = spawn(
        Path::new(&std::env::var("WORKSPACE_TEST_DBUS")?),
        &["--session", "--nofork", "--address", &address],
        &root,
        &display,
        &signature,
        "bus.log",
    )?;
    for _ in 0..100 {
        if root.join("bus").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let bus = zbus::connection::Builder::address(address.as_str())?
        .build()
        .await?;
    let _bar = spawn(
        Path::new(env!("CARGO_BIN_EXE_bar-daemon")),
        &["daemon"],
        &root,
        &display,
        &signature,
        "bar.log",
    )?;
    for _ in 0..150 {
        if call(&bus, "workspace.context", json!({})).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let qml = PathBuf::from(std::env::var("WORKSPACE_TEST_QML")?);
    let imports = qml.parent().unwrap().parent().unwrap().join("lib/qt-6/qml");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace-handoff.qml");

    for (index, mode, gap) in [
        (0, "stay", 200),
        (1, "background", 1200),
        (2, "manual", 200),
        (3, "close", 600),
        (4, "expired", 3000),
    ] {
        let control = root.join(format!("command-{index}"));
        fs::write(&control, "")?;
        let qt = spawn(
            &qml,
            &[
                "-I",
                imports.to_str().unwrap(),
                fixture.to_str().unwrap(),
                "--",
                control.to_str().unwrap(),
            ],
            &root,
            &display,
            &signature,
            &format!("qt-{index}.log"),
        )?;
        let first = window(&client, qt.0.id(), None).await?;
        let context =
            call(&bus, "workspace.context", json!({})).await?["workspace_operation"]["context"]
                .clone();
        let reservation = call(
            &bus,
            "workspace.create",
            json!({"request_id":format!("handoff-{index}"),"context":context,
            "purpose":"launch","workspace_id":null,"origin":"Qt handoff","icon":"test"}),
        )
        .await?["workspace_operation"]
            .clone();
        let destination = reservation["lease"]["workspace_id"].as_i64().unwrap();
        call(
            &bus,
            "workspace.placement.apply",
            json!({"lease":reservation["lease"],"window":{"address":first.address,
            "pid":first.pid,"workspace_id":first.workspace.id},"follow":true}),
        )
        .await?;
        call(
            &bus,
            "workspace.lease.release",
            reservation["lease"].clone(),
        )
        .await?;
        window(&client, qt.0.id(), Some(destination)).await?;
        // A user's login can take arbitrarily long; the grace starts at unmap.
        if mode == "stay" {
            tokio::time::sleep(Duration::from_millis(2300)).await;
        }
        if mode == "manual" {
            workspace::execute(
                &client,
                &workspace::guarded_move(&first.address, first.pid, destination, 1)?,
            )
            .await?;
        }
        if matches!(mode, "manual" | "background") {
            workspace::execute(
                &client,
                "local r=hl.dispatch(hl.dsp.focus({workspace=1})); assert(r.ok,r.error)",
            )
            .await?;
        }
        fs::write(&control, format!("replace:{gap}"))?;
        let mut unmapped = false;
        for _ in 0..50 {
            if !workspace::snapshot(&client)
                .await?
                .windows
                .iter()
                .any(|w| w.pid == qt.0.id())
            {
                unmapped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        ensure!(unmapped, "Qt fixture did not unmap its login window");
        if mode == "close" {
            call(
                &bus,
                "workspace.command",
                json!({"command":"close","request_id":format!("close-{index}")}),
            )
            .await?;
        }
        tokio::time::sleep(Duration::from_millis(gap + 300)).await;
        let expected = if matches!(mode, "stay" | "background") {
            Some(destination)
        } else {
            None
        };
        let next = window(&client, qt.0.id(), expected).await?;
        if matches!(mode, "manual" | "close" | "expired") {
            ensure!(next.workspace.id != destination, "{mode} revived placement");
        }
        if mode == "background" {
            ensure!(
                workspace::snapshot(&client)
                    .await?
                    .monitors
                    .iter()
                    .any(|m| m.focused && m.active_workspace.id == 1),
                "replacement stole focus"
            );
        }
        fs::write(&control, "exit")?;
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let state = call(
            &bus,
            "bar.snapshot",
            json!({"streams":["workspaces.changed"]}),
        )
        .await?;
        let monitors = state["snapshot"]["workspaces"]["lifecycle"]["monitors"]
            .as_object()
            .context("workspace projection unavailable")?;
        ensure!(
            !monitors.values().any(|items| items
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["id"] == destination))),
            "workspace did not retire after Qt exited: {state}"
        );
        drop(qt);
        eprintln!("Qt handoff scenario passed: {mode}, gap={gap}ms");
    }
    Ok(())
}
