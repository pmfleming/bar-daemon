//! Runtime-only hibernate settings are leased, not sticky configuration.
//! Never replace/delete an administrator-owned file; verify effective precedence.
use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{io::AsyncReadExt, sync::Mutex as AsyncMutex};

const NAME: &str = "90-shelllist.conf";
const HEADER: &str = "# Managed by bar-daemon\n";
static SERIAL: LazyLock<Arc<AsyncMutex<()>>> = LazyLock::new(|| Arc::new(AsyncMutex::new(())));

fn contents(minutes: u32) -> String {
    format!("{HEADER}[Sleep]\nHibernateDelaySec={minutes}min\nHibernateOnACPower=yes\n")
}
fn owned(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Some(value) = text.lines().find_map(|l| {
        l.strip_prefix("HibernateDelaySec=")
            .and_then(|v| v.strip_suffix("min"))
    }) else {
        return false;
    };
    value
        .parse::<u32>()
        .is_ok_and(|m| (1..=super::MAX_MINUTES).contains(&m) && text == contents(m))
}
pub(super) fn install(directory: &Path, minutes: u32) -> Result<Vec<u8>> {
    anyhow::ensure!(
        (1..=super::MAX_MINUTES).contains(&minutes),
        "hibernate delay must be 1–10080 minutes"
    );
    let path = directory.join(NAME);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            anyhow::ensure!(
                meta.file_type().is_file(),
                "refusing non-regular runtime sleep settings"
            );
            anyhow::ensure!(
                owned(&std::fs::read(&path)?),
                "runtime sleep settings are administrator-owned; refusing replacement"
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let bytes = contents(minutes).into_bytes();
    crate::paths::save_bytes_durable(&path, &bytes)?;
    Ok(bytes)
}
fn remove_if_unchanged(path: &Path, expected: &[u8]) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e.into()),
        _ => {}
    }
    if std::fs::read(path)? != expected || !owned(expected) {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    std::fs::File::open(path.parent().context("runtime settings parent")?)?.sync_all()?;
    Ok(true)
}
fn verify_effective(text: &str, minutes: u32) -> Result<()> {
    let mut section = "";
    let (mut delay, mut ac) = (None, None);
    for line in text.lines().map(str::trim) {
        if line.starts_with(['#', ';']) || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            section = line;
            continue;
        }
        if section != "[Sleep]" {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "HibernateDelaySec" => delay = Some(value.trim()),
                "HibernateOnACPower" => ac = Some(value.trim()),
                _ => {}
            }
        }
    }
    anyhow::ensure!(
        delay == Some(format!("{minutes}min").as_str()) && ac == Some("yes"),
        "administrator drop-in overrides managed hibernation (effective delay {delay:?}, AC setting {ac:?}); adjust system policy explicitly"
    );
    Ok(())
}
async fn effective(minutes: u32) -> Result<()> {
    crate::sleep::bounded(
        "read effective systemd sleep settings",
        Duration::from_secs(2),
        async {
            let output = tokio::process::Command::new("systemd-analyze")
                .args(["cat-config", "systemd/sleep.conf"])
                .kill_on_drop(true)
                .output()
                .await?;
            anyhow::ensure!(
                output.status.success(),
                "systemd-analyze could not read effective sleep settings"
            );
            verify_effective(std::str::from_utf8(&output.stdout)?, minutes)
        },
    )
    .await
}

// Read-only and deliberately conservative. A bus failure is not evidence that
// a settings lease is safe to release while systemd may still be consuming it.
async fn safe_to_restore() -> Result<bool> {
    crate::sleep::bounded("inspect sleep jobs", Duration::from_secs(3), async {
        let connection = crate::sleep::system_bus().await?;
        if crate::sleep::manager(&connection)
            .await?
            .get_property::<bool>("PreparingForSleep")
            .await?
        {
            return Ok(false);
        }
        let manager = zbus::Proxy::new(
            &connection,
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
        )
        .await?;
        for unit in [
            "systemd-suspend.service",
            "systemd-hibernate.service",
            "systemd-suspend-then-hibernate.service",
            "systemd-hybrid-sleep.service",
        ] {
            let path: zvariant::OwnedObjectPath = match manager.call("GetUnit", &(unit,)).await {
                Ok(path) => path,
                Err(zbus::Error::MethodError(name, _, _))
                    if name.as_str() == "org.freedesktop.systemd1.NoSuchUnit" =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let unit = zbus::Proxy::new(
                &connection,
                "org.freedesktop.systemd1",
                path,
                "org.freedesktop.systemd1.Unit",
            )
            .await?;
            let active: String = unit.get_property("ActiveState").await?;
            let (job, _): (u32, zvariant::OwnedObjectPath) = unit.get_property("Job").await?;
            if job != 0 || !matches!(active.as_str(), "inactive" | "failed") {
                return Ok(false);
            }
        }
        Ok(true)
    })
    .await
}

struct PendingOverride {
    path: PathBuf,
    bytes: Vec<u8>,
    armed: bool,
}
impl Drop for PendingOverride {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = remove_if_unchanged(&self.path, &self.bytes) {
                tracing::warn!(%error, "cancelled hibernate setup cleanup deferred to maintenance");
            }
        }
    }
}

pub(crate) async fn acquire(minutes: u32) -> Result<zvariant::OwnedFd> {
    let guard = SERIAL
        .clone()
        .try_lock_owned()
        .context("hibernate settings are already leased")?;
    anyhow::ensure!(safe_to_restore().await?, "a sleep job is already active");
    let (mut peer, fd) = tokio::net::UnixStream::pair()?;
    let fd = std::os::fd::OwnedFd::from(fd.into_std()?);
    let directory = Path::new("/run/systemd/sleep.conf.d");
    let mut pending = PendingOverride {
        path: directory.join(NAME),
        bytes: install(directory, minutes)?,
        armed: true,
    };
    if let Err(error) = effective(minutes).await {
        remove_if_unchanged(&pending.path, &pending.bytes)
            .context("remove rejected runtime override")?;
        return Err(error);
    }
    pending.armed = false;
    tokio::spawn(async move {
        let _exclusive = guard;
        let started = std::time::Instant::now();
        let mut byte = [0];
        // EOF on caller death, normal completion or cancelled preflight. Also
        // reclaim an abandoned lease after 120 awake seconds with no sleep job.
        let _ = tokio::time::timeout(Duration::from_secs(120), peer.read(&mut byte)).await;
        let settle = Duration::from_secs(30).saturating_sub(started.elapsed());
        tokio::time::sleep(settle).await;
        loop {
            if matches!(safe_to_restore().await, Ok(true)) {
                match remove_if_unchanged(&pending.path, &pending.bytes) {
                    Ok(_) => break,
                    Err(error) => {
                        tracing::warn!(%error, "runtime hibernate cleanup failed; retrying")
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });
    Ok(fd.into())
}

/// Called at helper startup and explicitly when managed hibernation is disabled.
/// Only exact generated content is eligible. A live lease always wins.
pub(crate) async fn cleanup() -> Result<bool> {
    let Ok(_guard) = SERIAL.try_lock() else {
        return Ok(false);
    };
    if !safe_to_restore().await? {
        return Ok(false);
    }
    let path = PathBuf::from("/run/systemd/sleep.conf.d").join(NAME);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e.into()),
    };
    if !owned(&bytes) {
        bail!("runtime override is not owned by bar-daemon; leaving it untouched");
    }
    remove_if_unchanged(&path, &bytes)
}

pub(crate) async fn maintain() {
    loop {
        if let Err(error) = cleanup().await {
            tracing::debug!(%error, "orphaned sleep settings left untouched");
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{NAME, PendingOverride, contents, install, remove_if_unchanged, verify_effective};
    #[test]
    fn only_exact_owned_and_unchanged_overrides_can_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(NAME);
        let bytes = install(dir.path(), 120).unwrap();
        assert!(verify_effective(std::str::from_utf8(&bytes).unwrap(), 120).is_ok());
        assert!(install(dir.path(), 0).is_err());
        assert!(install(dir.path(), super::super::MAX_MINUTES + 1).is_err());
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .contains("HibernateDelaySec=120min\nHibernateOnACPower=yes")
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(remove_if_unchanged(&path, &bytes).unwrap());
        std::fs::write(&path, "# admin\n[Sleep]\nHibernateDelaySec=5h\n").unwrap();
        assert!(install(dir.path(), 120).is_err());
        assert!(!remove_if_unchanged(&path, &bytes).unwrap());
    }
    #[test]
    fn cancelled_setup_removes_only_its_file_and_symlinks_are_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(NAME);
        let bytes = install(dir.path(), 15).unwrap();
        drop(PendingOverride {
            path: path.clone(),
            bytes,
            armed: true,
        });
        assert!(!path.exists());
        let target = dir.path().join("admin.conf");
        std::fs::write(&target, contents(15)).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(install(dir.path(), 30).is_err());
        assert!(!remove_if_unchanged(&path, contents(15).as_bytes()).unwrap());
        assert!(target.exists());
    }

    #[test]
    fn effective_readback_detects_higher_priority_overrides_and_resets() {
        let ours = contents(120);
        for extra in [
            "[Sleep]\nHibernateDelaySec=20min\n",
            "[Sleep]\nHibernateOnACPower=no\n",
            "[Sleep]\nHibernateDelaySec=\n",
        ] {
            assert!(verify_effective(&(ours.clone() + extra), 120).is_err());
        }
        assert!(verify_effective(&(ours + "[Other]\nHibernateDelaySec=20min\n"), 120).is_ok());
    }
}
