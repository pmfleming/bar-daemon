//! Process ownership and failure isolation on a private D-Bus and private runtime.
use anyhow::{Context, Result, ensure};
use std::{os::unix::fs::PermissionsExt, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    time::{sleep, timeout},
};

struct Session {
    root: tempfile::TempDir,
    _bus: Child,
    address: String,
    connection: zbus::Connection,
}
impl Session {
    async fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("run"))?;
        std::fs::set_permissions(
            root.path().join("run"),
            std::fs::Permissions::from_mode(0o700),
        )?;
        let mut bus = Command::new("dbus-daemon")
            .args([
                "--config-file",
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/test_support/dbus-session.conf"
                ),
                "--nofork",
                "--print-address=1",
            ])
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let address = timeout(
            Duration::from_secs(5),
            BufReader::new(bus.stdout.take().context("bus stdout")?)
                .lines()
                .next_line(),
        )
        .await??
        .context("bus address")?;
        let connection = zbus::connection::Builder::address(address.as_str())?
            .method_timeout(Duration::from_secs(5))
            .build()
            .await?;
        Ok(Self {
            root,
            _bus: bus,
            address,
            connection,
        })
    }
    fn start(&self, binary: &Path) -> Result<Child> {
        Ok(Command::new(binary)
            .arg("daemon")
            .env_clear()
            .env("HOME", self.root.path())
            .env("XDG_RUNTIME_DIR", self.root.path().join("run"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env(
                "BAR_DAEMON_BACKLIGHT_ROOT",
                self.root.path().join("backlight"),
            )
            .env("BAR_DAEMON_UPDATE_DESKTOP", "0")
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .env("DBUS_SYSTEM_BUS_ADDRESS", &self.address)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?)
    }
    async fn ready(&self, bus: &str) -> Result<()> {
        let proxy = zbus::Proxy::new(
            &self.connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await?;
        timeout(Duration::from_secs(20), async {
            loop {
                if proxy.call::<_, _, bool>("NameHasOwner", &(bus,)).await? {
                    return Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        Ok(())
    }
    async fn snapshot(&self, bus: &str) -> Result<serde_json::Value> {
        let proxy = zbus::Proxy::new(
            &self.connection,
            bus,
            "/org/laufan/BarDaemon",
            "org.laufan.BarDaemon2",
        )
        .await?;
        // Type=dbus readiness precedes interface export by a few awaits.
        timeout(Duration::from_secs(20), async {
            loop {
                if let Ok(reply) = proxy
                    .call::<_, _, String>("Call", &("bar.snapshot", "{}"))
                    .await
                {
                    return Ok::<_, anyhow::Error>(serde_json::from_str(&reply)?);
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await?
    }
}

fn assert_no_audio_controller(process: &Child) -> Result<()> {
    let pid = process.id().context("running process")?;
    for thread in std::fs::read_dir(format!("/proc/{pid}/task"))? {
        let name = std::fs::read_to_string(thread?.path().join("comm"))?;
        ensure!(
            !name.starts_with("bar-pipewire"),
            "foreign process started an audio controller: {name}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn notification_startup_failure_leaves_bar_available() -> Result<()> {
    let session = Session::new().await?;
    session
        .connection
        .request_name("org.freedesktop.Notifications")
        .await?;
    let mut failed = session.start(Path::new(env!("CARGO_BIN_EXE_bar-notification-daemon")))?;
    ensure!(
        !timeout(Duration::from_secs(20), failed.wait())
            .await??
            .success()
    );
    let mut bar = session.start(Path::new(env!("CARGO_BIN_EXE_bar-daemon")))?;
    session.ready("org.laufan.BarDaemon").await?;
    let snapshot = session.snapshot("org.laufan.BarDaemon").await?;
    ensure!(snapshot["ok"] == true, "{snapshot}");
    ensure!(snapshot["data"]["snapshot"].get("notifications").is_none());
    ensure!(
        !session
            .root
            .path()
            .join("state/bar-daemon/notifications-turso-v2.db")
            .exists()
    );
    bar.kill().await?;
    Ok(())
}

#[tokio::test]
async fn power_remains_available_after_notification_owner_dies() -> Result<()> {
    let session = Session::new().await?;
    let mut notifications =
        session.start(Path::new(env!("CARGO_BIN_EXE_bar-notification-daemon")))?;
    let mut power = session.start(Path::new(env!("CARGO_BIN_EXE_bar-power-daemon")))?;
    session.ready("org.laufan.BarNotificationDaemon").await?;
    session.ready("org.laufan.BarPowerDaemon").await?;
    let before = session.snapshot("org.laufan.BarPowerDaemon").await?;
    assert_no_audio_controller(&power)?;
    ensure!(before["ok"] == true, "{before}");
    ensure!(
        before["data"]["snapshot"]
            .as_object()
            .context("power snapshot")?
            .len()
            == 4
    );
    ensure!(!session.root.path().join("state/bar-daemon/agenda").exists());
    notifications.kill().await?;
    let after = session.snapshot("org.laufan.BarPowerDaemon").await?;
    ensure!(after["ok"] == true, "{after}");
    ensure!(
        power.try_wait()?.is_none(),
        "power stopped with notifications"
    );
    power.kill().await?;
    Ok(())
}

#[tokio::test]
async fn compositor_owner_rejects_power_calls_and_its_death_leaves_power_available() -> Result<()> {
    let session = Session::new().await?;
    let mut compositor = session.start(Path::new(env!("CARGO_BIN_EXE_bar-compositor-daemon")))?;
    let mut power = session.start(Path::new(env!("CARGO_BIN_EXE_bar-power-daemon")))?;
    session.ready("org.laufan.BarCompositorDaemon").await?;
    session.ready("org.laufan.BarPowerDaemon").await?;
    let state = session.snapshot("org.laufan.BarCompositorDaemon").await?;
    ensure!(
        state["data"]["snapshot"]
            .as_object()
            .context("compositor snapshot")?
            .len()
            == 4
    );
    let proxy = zbus::Proxy::new(
        &session.connection,
        "org.laufan.BarCompositorDaemon",
        "/org/laufan/BarDaemon",
        "org.laufan.BarDaemon2",
    )
    .await?;
    let reply: String = proxy
        .call("Call", &("powerProfile.set", "{\"profile\":\"balanced\"}"))
        .await?;
    ensure!(serde_json::from_str::<serde_json::Value>(&reply)?["ok"] == false);
    let reply: String = proxy.call("Subscribe", &(vec!["battery.changed"],)).await?;
    ensure!(serde_json::from_str::<serde_json::Value>(&reply)?["ok"] == false);
    compositor.kill().await?;
    ensure!(session.snapshot("org.laufan.BarPowerDaemon").await?["ok"] == true);
    ensure!(power.try_wait()?.is_none());
    power.kill().await?;
    Ok(())
}

#[tokio::test]
async fn activity_restarts_without_resetting_power_or_bar() -> Result<()> {
    let session = Session::new().await?;
    let mut bar = session.start(Path::new(env!("CARGO_BIN_EXE_bar-daemon")))?;
    let mut power = session.start(Path::new(env!("CARGO_BIN_EXE_bar-power-daemon")))?;
    session.ready("org.laufan.BarDaemon").await?;
    session.snapshot("org.laufan.BarDaemon").await?;
    session.ready("org.laufan.BarPowerDaemon").await?;
    let before = session.snapshot("org.laufan.BarPowerDaemon").await?;
    assert_no_audio_controller(&power)?;
    ensure!(!session.root.path().join("state/bar-daemon/agenda").exists());
    let mut activity = session.start(Path::new(env!("CARGO_BIN_EXE_bar-activity-daemon")))?;
    session.ready("org.laufan.BarActivityDaemon").await?;
    let initial = session.snapshot("org.laufan.BarActivityDaemon").await?;
    assert_no_audio_controller(&activity)?;
    ensure!(
        initial["data"]["snapshot"]
            .as_object()
            .context("activity snapshot")?
            .len()
            == 2
    );
    let proxy = zbus::Proxy::new(
        &session.connection,
        "org.laufan.BarActivityDaemon",
        "/org/laufan/BarDaemon",
        "org.laufan.BarDaemon2",
    )
    .await?;
    let list: String = proxy.call("Call", &("agenda.list", "{}")).await?;
    ensure!(
        serde_json::from_str::<serde_json::Value>(&list)?["ok"] == true,
        "{list}"
    );
    activity.kill().await?;
    let mut replacement = session.start(Path::new(env!("CARGO_BIN_EXE_bar-activity-daemon")))?;
    session.ready("org.laufan.BarActivityDaemon").await?;
    let restarted = session.snapshot("org.laufan.BarActivityDaemon").await?;
    ensure!(
        restarted["data"]["snapshot"]["activity"]["_publication"]["epoch"]
            != initial["data"]["snapshot"]["activity"]["_publication"]["epoch"]
    );
    let after = session.snapshot("org.laufan.BarPowerDaemon").await?;
    ensure!(
        after["data"]["snapshot"]["battery"]["_publication"]["epoch"]
            == before["data"]["snapshot"]["battery"]["_publication"]["epoch"]
    );
    ensure!(bar.try_wait()?.is_none() && power.try_wait()?.is_none());
    replacement.kill().await?;
    power.kill().await?;
    bar.kill().await?;
    Ok(())
}

#[tokio::test]
async fn failed_notification_store_never_advertises_public_readiness() -> Result<()> {
    use futures::StreamExt;
    let session = Session::new().await?;
    std::fs::create_dir_all(
        session
            .root
            .path()
            .join("state/bar-daemon/notifications-turso-v2.db"),
    )?;
    let proxy = zbus::Proxy::new(
        &session.connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await?;
    let mut changes = proxy.receive_signal("NameOwnerChanged").await?;
    let mut process = session.start(Path::new(env!("CARGO_BIN_EXE_bar-notification-daemon")))?;
    ensure!(
        !timeout(Duration::from_secs(20), process.wait())
            .await??
            .success()
    );
    while let Ok(Some(message)) = timeout(Duration::from_millis(100), changes.next()).await {
        let (name, _, new): (String, String, String) = message.body().deserialize()?;
        ensure!(
            new.is_empty()
                || !matches!(
                    name.as_str(),
                    "org.laufan.BarNotificationDaemon" | "org.freedesktop.Notifications"
                ),
            "failed startup advertised {name}"
        );
    }
    Ok(())
}
