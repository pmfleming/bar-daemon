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
            .find(|w| {
                w.mapped && w.pid == pid && workspace.is_none_or(|id| w.workspace.id == Some(id))
            })
        {
            return Ok(w);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("Qt window did not appear on {workspace:?}")
}

async fn workspace_icon(bus: &zbus::Connection, id: i64, expected: &str) -> Result<()> {
    for _ in 0..80 {
        let state = call(
            bus,
            "bar.snapshot",
            json!({"streams":["workspaces.changed"]}),
        )
        .await?;
        if state["snapshot"]["workspaces"]["lifecycle"]["monitors"]
            .as_object()
            .is_some_and(|monitors| {
                monitors.values().any(|items| {
                    items.as_array().is_some_and(|items| {
                        items
                            .iter()
                            .any(|item| item["id"] == id && item["icon"] == expected)
                    })
                })
            })
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("workspace {id} did not adopt icon {expected}")
}

async fn workspace_order(bus: &zbus::Connection, expected: &[i64]) -> Result<()> {
    for _ in 0..100 {
        let state = call(
            bus,
            "bar.snapshot",
            json!({"streams":["workspaces.changed"]}),
        )
        .await?;
        if state["snapshot"]["workspaces"]["lifecycle"]["monitors"]
            .as_object()
            .is_some_and(|monitors| {
                monitors.values().any(|items| {
                    items.as_array().is_some_and(|items| {
                        items
                            .iter()
                            .filter_map(|item| item["id"].as_i64())
                            .collect::<Vec<_>>()
                            == expected
                    })
                })
            })
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("workspace bar did not reach order {expected:?}")
}

fn qt_runner(root: &Path, qml: &Path) -> Result<PathBuf> {
    let declarative = qml.parent().unwrap().parent().unwrap();
    let plugins = PathBuf::from(std::env::var("WORKSPACE_TEST_QT_PLUGINS")?);
    let base = plugins
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let binary = root.join("workspace-qt-runner");
    let mut compiler =
        Command::new(std::env::var("WORKSPACE_TEST_CXX").unwrap_or_else(|_| "c++".into()));
    compiler.args(["-std=c++17", "-fPIC"]);
    for prefix in [base, declarative] {
        compiler.arg(format!("-I{}", prefix.join("include").display()));
        compiler.arg(format!("-L{}", prefix.join("lib").display()));
        compiler.arg(format!("-Wl,-rpath,{}", prefix.join("lib").display()));
    }
    let status = compiler
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace-runner.cpp"))
        .args(["-lQt6Qml", "-lQt6Gui", "-lQt6Core", "-o"])
        .arg(&binary)
        .status()?;
    ensure!(
        status.success(),
        "compile private Qt runner (requires C++ and Qt development headers)"
    );
    Ok(binary)
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
    let qml = PathBuf::from(std::env::var("WORKSPACE_TEST_QML")?);
    let imports = qml.parent().unwrap().parent().unwrap().join("lib/qt-6/qml");
    let qml = qt_runner(&root, &qml)?;
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace-handoff.qml");

    // Numeric selection must not implicitly allocate extra destinations.
    let control = root.join("numeric-command");
    fs::write(&control, "")?;
    let qt = spawn(
        &qml,
        &[
            "-I",
            imports.to_str().unwrap(),
            fixture.to_str().unwrap(),
            "--",
            "icon-app-first",
            control.to_str().unwrap(),
        ],
        &root,
        &display,
        &signature,
        "qt-numeric.log",
    )?;
    let first = window(&client, qt.0.id(), None).await?;
    let second_control = root.join("second-command");
    fs::write(&second_control, "")?;
    let second_qt = spawn(
        &qml,
        &[
            "-I",
            imports.to_str().unwrap(),
            fixture.to_str().unwrap(),
            "--",
            "icon-app-second",
            second_control.to_str().unwrap(),
        ],
        &root,
        &display,
        &signature,
        "qt-second.log",
    )?;
    let second = window(&client, second_qt.0.id(), None).await?;
    ensure!(
        first.class != second.class && !first.class.is_empty(),
        "Qt application identities are not distinct: {first:?} {second:?}"
    );
    fs::create_dir_all(root.join("data/applications"))?;
    for (name, class) in [("first", &first.class), ("second", &second.class)] {
        fs::write(
            root.join(format!("data/applications/{name}.desktop")),
            format!(
                "[Desktop Entry]\nType=Application\nName={name}\nStartupWMClass={class}\nIcon=fixture-{name}\nExec=true\n"
            ),
        )?;
    }
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
    workspace::execute(
        &client,
        &workspace::focus_window(&first.address, first.pid, first.workspace.id.unwrap())?,
    )
    .await?;
    for command in ["focus", "move"] {
        ensure!(
            call(
                &bus,
                "workspace.command",
                json!({"command":command,"workspace_id":6})
            )
            .await
            .is_err(),
            "absent destination accepted"
        );
        ensure!(
            !workspace::snapshot(&client)
                .await?
                .workspaces
                .iter()
                .any(|w| w.id == Some(6)),
            "numeric command created workspace 6"
        );
        window(&client, qt.0.id(), first.workspace.id).await?;
    }
    call(
        &bus,
        "workspace.command",
        json!({"command":"focus","workspace_id":5}),
    )
    .await?;
    let manual = call(
        &bus,
        "workspace.command",
        json!({"command":"create","request_id":"numeric-manual"}),
    )
    .await?;
    let destination = manual["workspace_operation"]["lease"]["workspace_id"]
        .as_i64()
        .unwrap();
    ensure!(
        destination == 6,
        "rejected shortcuts consumed an allocation"
    );
    call(
        &bus,
        "workspace.command",
        json!({"command":"focus","workspace_id":first.workspace.id.unwrap()}),
    )
    .await?;
    call(
        &bus,
        "workspace.command",
        json!({"command":"move","workspace_id":destination}),
    )
    .await?;
    window(&client, qt.0.id(), Some(destination)).await?;
    workspace_icon(&bus, destination, "fixture-first").await?;
    call(
        &bus,
        "workspace.command",
        json!({"command":"move","workspace_id":1}),
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    ensure!(
        !workspace::snapshot(&client)
            .await?
            .workspaces
            .iter()
            .any(|w| w.id == Some(destination)),
        "fixture did not release empty compositor workspace"
    );
    // Both commands can rematerialize a retained logical destination.
    call(
        &bus,
        "workspace.command",
        json!({"command":"focus","workspace_id":destination}),
    )
    .await?;
    call(
        &bus,
        "workspace.command",
        json!({"command":"focus","workspace_id":1}),
    )
    .await?;
    call(
        &bus,
        "workspace.command",
        json!({"command":"move","workspace_id":destination}),
    )
    .await?;
    window(&client, qt.0.id(), Some(destination)).await?;
    let snapshot = workspace::snapshot(&client).await?;
    ensure!(
        snapshot
            .monitors
            .iter()
            .any(|m| m.focused && m.active_workspace.id == Some(destination)),
        "move did not follow"
    );
    ensure!(
        !snapshot
            .workspaces
            .iter()
            .any(|w| w.id.is_some_and(|id| id > 6)),
        "move created a spare workspace"
    );
    workspace::execute(
        &client,
        &workspace::focus_window(&second.address, second.pid, 1)?,
    )
    .await?;
    call(
        &bus,
        "workspace.command",
        json!({"command":"move","workspace_id":destination}),
    )
    .await?;
    window(&client, second.pid, Some(destination)).await?;
    workspace_icon(&bus, destination, "fixture-first").await?;
    call(
        &bus,
        "workspace.command",
        json!({"command":"move","workspace_id":1}),
    )
    .await?;
    workspace::execute(
        &client,
        &workspace::focus_window(&first.address, first.pid, destination)?,
    )
    .await?;
    call(
        &bus,
        "workspace.command",
        json!({"command":"move","workspace_id":1}),
    )
    .await?;
    workspace::execute(
        &client,
        &workspace::focus_window(&second.address, second.pid, 1)?,
    )
    .await?;
    call(
        &bus,
        "workspace.command",
        json!({"command":"move","workspace_id":destination}),
    )
    .await?;
    workspace_icon(&bus, destination, "fixture-second").await?;
    workspace_icon(&bus, 1, "terminal").await?;
    eprintln!(
        "Workspace icon scenarios passed: first occupant, stable while occupied, new icon after empty, fixed standard icon"
    );
    call(
        &bus,
        "workspace.command",
        json!({"command":"close","request_id":"numeric-close"}),
    )
    .await?;
    drop(qt);
    drop(second_qt);
    eprintln!(
        "Numeric shortcut scenarios passed: absent, standard, retained, move-and-follow, no spare workspace"
    );

    // Real Qt focus/move through gapped bar positions. Retire IDs 7 and 8,
    // retain empty 9, and select ID 10 as the seventh bar destination.
    workspace_order(&bus, &[1, 2, 3, 4, 5]).await?;
    let mut destinations = Vec::new();
    for index in 0..4 {
        let created = call(
            &bus,
            "workspace.command",
            json!({
                "command":"create","request_id":format!("position-create-{index}")
            }),
        )
        .await?;
        let id = created["workspace_operation"]["lease"]["workspace_id"]
            .as_i64()
            .unwrap();
        destinations.push(id);
        if index < 2 {
            call(
                &bus,
                "workspace.command",
                json!({
                    "command":"close","request_id":format!("position-close-{index}")
                }),
            )
            .await?;
            workspace_order(&bus, &[1, 2, 3, 4, 5]).await?;
        }
    }
    ensure!(
        destinations == [7, 8, 9, 10],
        "unexpected positional fixture IDs"
    );
    workspace_order(&bus, &[1, 2, 3, 4, 5, 9, 10]).await?;
    call(
        &bus,
        "workspace.command",
        json!({"command":"focus","workspace_id":1}),
    )
    .await?;
    let position_control = root.join("position-command");
    fs::write(&position_control, "")?;
    let position_qt = spawn(
        &qml,
        &[
            "-I",
            imports.to_str().unwrap(),
            fixture.to_str().unwrap(),
            "--",
            "position-app",
            position_control.to_str().unwrap(),
        ],
        &root,
        &display,
        &signature,
        "qt-position.log",
    )?;
    let position_window = window(&client, position_qt.0.id(), Some(1)).await?;
    workspace::execute(
        &client,
        &workspace::focus_window(&position_window.address, position_window.pid, 1)?,
    )
    .await?;
    // Exercise the actual shortcut CLI as well as the owner API.
    let mut shortcut = spawn(
        Path::new(env!("CARGO_BIN_EXE_bar-daemon")),
        &["workspace", "focus", "7", "--position"],
        &root,
        &display,
        &signature,
        "position-cli.log",
    )?;
    let status = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(status) = shortcut.0.try_wait()? {
                return Ok::<_, anyhow::Error>(status);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    ensure!(status.success(), "positional shortcut CLI failed");
    ensure!(
        workspace::snapshot(&client)
            .await?
            .monitors
            .iter()
            .any(|m| { m.focused && m.active_workspace.id == Some(10) }),
        "seventh bar position did not focus workspace 10"
    );
    for position in [7, 1, 7, 1] {
        if position == 7 {
            workspace::execute(
                &client,
                &workspace::focus_window(&position_window.address, position_window.pid, 1)?,
            )
            .await?;
        }
        let reply = call(
            &bus,
            "workspace.command",
            json!({
                "command":"move","workspace_position":position
            }),
        )
        .await?;
        let expected = if position == 7 { 10 } else { 1 };
        ensure!(
            reply["workspace_operation"]["workspace_id"] == expected,
            "wrong position destination"
        );
        window(&client, position_window.pid, Some(expected)).await?;
        ensure!(
            workspace::snapshot(&client)
                .await?
                .monitors
                .iter()
                .any(|m| { m.focused && m.active_workspace.id == Some(expected) }),
            "positional move did not follow"
        );
    }
    call(
        &bus,
        "workspace.command",
        json!({"command":"focus","workspace_position":6}),
    )
    .await?;
    call(
        &bus,
        "workspace.command",
        json!({"command":"close","request_id":"position-close-retained"}),
    )
    .await?;
    workspace_order(&bus, &[1, 2, 3, 4, 5, 10]).await?;
    for command in ["focus", "move"] {
        ensure!(
            call(
                &bus,
                "workspace.command",
                json!({
                    "command":command,"workspace_position":7
                })
            )
            .await
            .is_err(),
            "missing position accepted"
        );
    }
    workspace::execute(
        &client,
        &workspace::focus_window(&position_window.address, position_window.pid, 1)?,
    )
    .await?;
    let moved = call(
        &bus,
        "workspace.command",
        json!({
            "command":"move","workspace_position":6
        }),
    )
    .await?;
    ensure!(
        moved["workspace_operation"]["workspace_id"] == 10,
        "position did not shift after close"
    );
    window(&client, position_window.pid, Some(10)).await?;
    call(
        &bus,
        "workspace.command",
        json!({"command":"close","request_id":"position-close-final"}),
    )
    .await?;
    drop(position_qt);
    workspace_order(&bus, &[1, 2, 3, 4, 5]).await?;
    ensure!(
        !workspace::snapshot(&client)
            .await?
            .workspaces
            .iter()
            .any(|w| w.id.is_some_and(|id| id > 10)),
        "positional shortcuts allocated a workspace"
    );
    eprintln!(
        "Positional shortcuts passed: seventh selects ID 10, native CLI, Qt move/follow, retained empty, shift after close, missing rejection"
    );

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
            "pid":first.pid,"workspace_id":first.workspace.id.unwrap()},"follow":true}),
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
                &shelllist_hyprland::lua::focus_workspace(1, false)?,
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
            ensure!(
                next.workspace.id != Some(destination),
                "{mode} revived placement"
            );
        }
        if mode == "background" {
            ensure!(
                workspace::snapshot(&client)
                    .await?
                    .monitors
                    .iter()
                    .any(|m| m.focused && m.active_workspace.id == Some(1)),
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
