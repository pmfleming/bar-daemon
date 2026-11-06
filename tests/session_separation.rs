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
