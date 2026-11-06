//! A private compositor replacement must use the daemon's cleanup/error return.
use anyhow::{Context, Result, ensure};
use std::{os::unix::fs::PermissionsExt, process::Stdio, time::Duration};
use tokio::{io::AsyncBufReadExt, net::UnixListener, process::Command, time::timeout};

#[tokio::test]
async fn compositor_replacement_exits_only_the_compositor_owner() -> Result<()> {
    let root = tempfile::Builder::new()
        .prefix("bar-session-")
        .tempdir_in("/tmp")?;
    let runtime = root.path().join("run");
    let instance = runtime.join("hypr/original");
    std::fs::create_dir_all(&instance)?;
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
    let _command = UnixListener::bind(instance.join(".socket.sock"))?;
    let event = UnixListener::bind(instance.join(".socket2.sock"))?;
    let config = root.path().join("bus.conf");
    std::fs::write(&config, include_str!("../test_support/dbus-session.conf"))?;
    let mut bus = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut address = String::new();
    timeout(
        Duration::from_secs(10),
        tokio::io::BufReader::new(bus.stdout.take().context("bus stdout")?).read_line(&mut address),
    )
    .await??;
    let log_path = root.path().join("daemon.log");
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_bar-compositor-daemon"));
    daemon
        .arg("daemon")
        .env_clear()
        .env("HOME", root.path())
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("DBUS_SESSION_BUS_ADDRESS", address.trim())
        .env("DBUS_SYSTEM_BUS_ADDRESS", address.trim())
        .env("HYPRLAND_INSTANCE_SIGNATURE", "original")
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log_path)?)
        .kill_on_drop(true);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        daemon.env("LLVM_PROFILE_FILE", profile);
    }
    let process = daemon.spawn()?;
    let (stream, _) = timeout(Duration::from_secs(30), event.accept())
        .await
        .with_context(|| {
            format!(
                "compositor watcher did not start: {}",
                std::fs::read_to_string(&log_path).unwrap_or_default()
            )
        })??;
    // The listener remains live at its renamed path, but the configured session
    // no longer exists. Diagnostic discovery may connect without rebinding.
    std::fs::rename(&instance, runtime.join("hypr/replacement"))?;
    drop(stream);
    let output = timeout(Duration::from_secs(30), process.wait_with_output()).await??;
    ensure!(!output.status.success(), "session loss exited successfully");
    let error = std::fs::read_to_string(&log_path)?;
    ensure!(
        error.contains("session original is gone") && error.contains("replacement"),
        "unexpected failure: {error}"
    );
    ensure!(
        !root
            .path()
            .join("state/bar-daemon/notifications-turso-v2.db")
            .exists(),
        "bar process opened the notification store"
    );
    bus.kill().await?;
    bus.wait().await?;
    Ok(())
}
