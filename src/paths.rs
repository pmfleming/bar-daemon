use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};
use shelllist_daemon_core::{XdgRoot, resolve_xdg_path};

pub(crate) fn data_file(root: XdgRoot, name: &str) -> PathBuf {
    resolve_xdg_path(root, "bar-daemon", Path::new(name))
        .unwrap_or_else(|| PathBuf::from("bar-daemon").join(name))
}

pub(crate) async fn load_json_or_default<T>(path: &Path, subject: &str) -> Result<T>
where
    T: DeserializeOwned + Default,
{
    match tokio::fs::read(path).await {
        Ok(contents) => serde_json::from_slice(&contents)
            .with_context(|| format!("parse {subject} {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

pub(crate) async fn save_json_atomic<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize + ?Sized,
{
    let contents = serde_json::to_vec_pretty(value)?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || save_bytes_durable(&path, &contents))
        .await
        .context("join durable JSON write")?
}

fn parent_directory(path: &Path) -> Result<&Path> {
    let parent = path
        .parent()
        .with_context(|| format!("data path {} has no parent", path.display()))?;
    Ok(if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    })
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync directory {}", path.display()))
}

fn create_directory_durable(path: &Path) -> Result<()> {
    let missing = path
        .ancestors()
        .take_while(|path| !path.as_os_str().is_empty() && !path.exists())
        .collect::<Vec<_>>();
    fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    // On the first save, the state directory itself may also be new. Persist
    // its ancestors' directory entries, not just the final JSON rename.
    for directory in missing.into_iter().rev() {
        sync_directory(directory)?;
        sync_directory(parent_directory(directory)?)?;
    }
    Ok(())
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn temporary_file(path: &Path) -> Result<(File, TemporaryFile)> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().context("data path has no file name")?;
    loop {
        let mut temporary_name = name.to_os_string();
        temporary_name.push(format!(
            ".{}-{}.tmp",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = path.with_file_name(temporary_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((file, TemporaryFile(temporary))),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("create {}", temporary.display()));
            }
        }
    }
}

fn save_bytes_durable(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = parent_directory(path)?;
    create_directory_durable(parent)?;
    let (mut file, temporary) = temporary_file(path)?;
    file.write_all(contents)
        .with_context(|| format!("write {}", temporary.0.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", temporary.0.display()))?;
    fs::rename(&temporary.0, path).with_context(|| format!("replace {}", path.display()))?;
    // A successful return is the durability barrier before battery hardware
    // changes. Atomic rename alone does not survive power loss reliably.
    sync_directory(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn durable_json_save_creates_parents_and_replaces_existing_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("new/state/battery.json");
        save_json_atomic(&path, &vec![1, 2, 3]).await.unwrap();
        save_json_atomic(&path, &vec![4]).await.unwrap();
        assert_eq!(
            load_json_or_default::<Vec<u8>>(&path, "test")
                .await
                .unwrap(),
            vec![4]
        );
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn failed_replacement_preserves_target_and_removes_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("sentinel"), "keep").unwrap();
        assert!(save_bytes_durable(&target, b"replacement").is_err());
        assert_eq!(fs::read_to_string(target.join("sentinel")).unwrap(), "keep");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        assert_eq!(
            parent_directory(Path::new("relative.json")).unwrap(),
            Path::new(".")
        );
    }
}
