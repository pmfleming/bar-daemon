//! Validate and read the same opened inode; never follow final symlinks or wait on FIFOs.
use anyhow::{Result, ensure};
use std::{fs::File, io::Read, os::unix::fs::MetadataExt, path::Path};

pub(super) fn open(path: &Path, owner: u32) -> Result<File> {
    use rustix::fs::{Mode, OFlags};
    let file = File::from(
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == owner && metadata.mode() & 0o022 == 0,
        "Untrusted update file"
    );
    Ok(file)
}

pub(super) fn read(path: &Path, owner: u32, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open(path, owner)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "Update file exceeds read limit");
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::read;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn rejects_wrong_owner_writable_files_links_special_files_and_oversize_reads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("record");
        let owner = rustix::process::geteuid().as_raw();
        fs::write(&path, b"data").unwrap();
        assert_eq!(read(&path, owner, 4).unwrap(), b"data");
        assert!(read(&path, owner, 3).is_err());
        assert!(read(&path, owner ^ 1, 4).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(read(&path, owner, 4).is_err());
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/missing", &path).unwrap();
        assert!(read(&path, owner, 4).is_err());
        fs::remove_file(&path).unwrap();
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &path,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        assert!(read(&path, owner, 4).is_err());
        assert!(read(directory.path(), owner, 4).is_err());
    }
}
