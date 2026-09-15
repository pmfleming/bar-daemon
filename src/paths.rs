use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};
use shelllist_daemon_core::{AtomicFilePolicy, XdgRoot, resolve_xdg_path};

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

pub(crate) async fn save_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    shelllist_daemon_tokio::write_bytes_atomic_async(
        path.to_owned(),
        serde_json::to_vec_pretty(value)?,
        AtomicFilePolicy::DURABLE,
    )
    .await
}

/// This successful return remains the durability barrier before hardware changes.
pub(crate) fn save_bytes_durable(path: &Path, contents: &[u8]) -> Result<()> {
    shelllist_daemon_core::write_bytes_atomic(path, contents, AtomicFilePolicy::DURABLE)
        .with_context(|| format!("persist {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
    }
}
