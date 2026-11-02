//! Private Turso store. Only the notification owner thread may use this connection.
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use super::schema::{SCHEMA, VERSION};
use anyhow::{Context, Result, ensure};
use turso::Connection;

const APPLICATION_ID: i64 = 0x4e4f5446;

pub(super) struct NotificationStore {
    pub(super) connection: Connection,
    _database: turso::Database,
    _file: File,
    // Drop connection/database before releasing the process ownership lock.
    _ownership: Ownership,
    path: PathBuf,
    pub(super) blocked: bool,
    #[cfg(test)]
    pub(super) fault: Option<Fault>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fault {
    Save,
    Children,
    DeleteChildren,
    Prune,
    Commit,
    Rollback,
}

struct Ownership(File);
impl Drop for Ownership {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            tracing::warn!(%error, "notification ownership unlock failed");
        }
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn private_file(path: &Path, create: bool, exclusive: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .create_new(exclusive)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == rustix::process::geteuid().as_raw() && meta.nlink() == 1,
        "unsafe notification file {}",
        path.display()
    );
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

impl NotificationStore {
    pub(super) async fn open(path: &Path) -> Result<Self> {
        Self::open_inner(path).await.with_context(|| format!(
            "open notification Turso store {}; legacy/incompatible stores are not migrated, choose a fresh BAR_DAEMON_NOTIFICATION_DATABASE path", path.display()
        ))
    }

    async fn open_inner(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let meta = fs::symlink_metadata(parent)?;
        // A private parent also protects a newly created WAL before chmod.
        ensure!(
            meta.is_dir() && meta.uid() == rustix::process::geteuid().as_raw(),
            "notification database directory must be owned by this user and not a symlink"
        );
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        let lock = private_file(&sidecar(path, ".lock"), true, false)?;
        lock.try_lock()
            .context("notification database already owned")?;
        let ownership = Ownership(lock);
        let (file, fresh) = match private_file(path, false, true) {
            Ok(file) => (file, true),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) =>
            {
                (private_file(path, false, false)?, false)
            }
            Err(error) => return Err(error),
        };
        ensure!(
            fresh || file.metadata()?.len() > 0 || sidecar(path, "-wal").try_exists()?,
            "preexisting empty notification database; choose a fresh path"
        );
        for suffix in ["-wal", "-shm"] {
            let side = sidecar(path, suffix);
            match fs::symlink_metadata(&side) {
                Ok(_) => {
                    ensure!(!fresh, "orphaned notification sidecar; choose a fresh path");
                    drop(private_file(&side, false, false)?);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let database = turso::Builder::new_local(
            path.to_str()
                .context("non-UTF8 notification database path")?,
        )
        .with_io(turso::IoBackend::Syscall)
        .build()
        .await?;
        let held = file.metadata()?;
        let opened = fs::symlink_metadata(path)?;
        ensure!(
            opened.is_file() && opened.ino() == held.ino() && opened.dev() == held.dev(),
            "notification database changed while opening"
        );
        let connection = database.connect()?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection
            .execute_batch("PRAGMA synchronous=FULL; PRAGMA cache_size=-8192;")
            .await?;
        let mut store = Self {
            connection,
            _database: database,
            _file: file,
            _ownership: ownership,
            path: path.to_owned(),
            blocked: false,
            #[cfg(test)]
            fault: None,
        };
        if fresh {
            store.connection.execute("BEGIN IMMEDIATE", ()).await?;
            let initialized = async {
                store.connection.execute_batch(SCHEMA).await?;
                store
                    .connection
                    .execute_batch(format!(
                        "PRAGMA application_id={APPLICATION_ID}; PRAGMA user_version={VERSION};"
                    ))
                    .await?;
                Ok(())
            }
            .await;
            store.finish(initialized).await?;
        } else {
            ensure!(
                store.integer("PRAGMA application_id").await? == APPLICATION_ID
                    && store.integer("PRAGMA user_version").await? == VERSION,
                "unsupported notification database identity/schema"
            );
        }
        store.harden_sidecars()?;
        tracing::info!(
            path = %path.display(),
            engine = "turso",
            engine_version = "0.8.3",
            schema_version = VERSION,
            created = fresh,
            "notification store opened"
        );
        Ok(store)
    }

    pub(super) async fn integer(&self, sql: &str) -> Result<i64> {
        Ok(self
            .connection
            .query(sql, ())
            .await?
            .next()
            .await?
            .context("missing scalar")?
            .get(0)?)
    }

    fn harden_sidecars(&self) -> Result<()> {
        for suffix in ["-wal", "-shm"] {
            let path = sidecar(&self.path, suffix);
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    drop(private_file(&path, false, false)?);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn competing_connection(&self) -> turso::Connection {
        self._database.connect().unwrap()
    }

    #[cfg(test)]
    pub(super) fn fail(&mut self, fault: Fault) -> Result<()> {
        if self.fault == Some(fault) {
            self.fault = None;
            anyhow::bail!("injected notification {fault:?} failure");
        }
        Ok(())
    }

    pub(super) fn healthy(&self) -> Result<()> {
        ensure!(
            !self.blocked,
            "notification database fenced; restart required"
        );
        Ok(())
    }

    pub(super) async fn begin(&self) -> Result<()> {
        self.healthy()?;
        self.connection.execute("BEGIN IMMEDIATE", ()).await?;
        Ok(())
    }

    async fn commit(&mut self) -> Result<()> {
        self.connection.execute("COMMIT", ()).await?;
        // Simulate an error after COMMIT became durable: never replay it.
        #[cfg(test)]
        self.fail(Fault::Commit)?;
        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        #[cfg(test)]
        self.fail(Fault::Rollback)?;
        self.connection.execute("ROLLBACK", ()).await?;
        Ok(())
    }

    pub(super) async fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        let result = match result {
            Ok(value) => match self.commit().await {
                Ok(_) => Ok(value),
                Err(error) => {
                    self.blocked = true;
                    Err(error)
                }
            },
            Err(error) => Err(error),
        };
        if result.is_err()
            && !self.connection.is_autocommit().unwrap_or(false)
            && self.rollback().await.is_err()
        {
            self.blocked = true;
        }
        if let Err(error) = self.harden_sidecars() {
            self.blocked = true;
            return Err(error).context("notification sidecar protection failed");
        }
        result
    }

    pub(super) async fn checkpoint(&self) -> Result<()> {
        self.healthy()?;
        let mut rows = self
            .connection
            .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
            .await?;
        let row = rows.next().await?.context("missing checkpoint result")?;
        ensure!(row.get::<i64>(0)? == 0, "notification checkpoint busy");
        ensure!(
            row.get::<i64>(1)? == row.get::<i64>(2)?,
            "notification checkpoint incomplete"
        );
        self.harden_sidecars()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_identity_reopen_and_ownership() {
        futures::executor::block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("notifications.db");
            let store = NotificationStore::open(&path).await.unwrap();
            assert!(NotificationStore::open(&path).await.is_err());
            assert_eq!(
                store.integer("PRAGMA application_id").await.unwrap(),
                APPLICATION_ID
            );
            store.checkpoint().await.unwrap();
            drop(store);
            let store = NotificationStore::open(&path).await.unwrap();
            assert_eq!(store.integer("PRAGMA user_version").await.unwrap(), VERSION);
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        });
    }

    #[test]
    fn refuses_existing_empty_foreign_and_corrupt_stores() {
        futures::executor::block_on(async {
            let dir = tempfile::tempdir().unwrap();
            for (name, bytes) in [("empty", &b""[..]), ("corrupt", &b"not a database"[..])] {
                let path = dir.path().join(name);
                fs::write(&path, bytes).unwrap();
                assert!(NotificationStore::open(&path).await.is_err());
                assert_eq!(fs::read(&path).unwrap(), bytes);
            }
            let path = dir.path().join("foreign");
            let store = NotificationStore::open(&path).await.unwrap();
            store
                .connection
                .execute("PRAGMA application_id=123", ())
                .await
                .unwrap();
            store.checkpoint().await.unwrap();
            drop(store);
            assert!(NotificationStore::open(&path).await.is_err());
        });
    }
}
