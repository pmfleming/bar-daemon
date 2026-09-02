use std::path::{Path, PathBuf};

use shelllist_daemon_core::{XdgRoot, resolve_xdg_path};

pub(crate) fn data_file(root: XdgRoot, name: &str) -> PathBuf {
    resolve_xdg_path(root, "bar-daemon", Path::new(name))
        .unwrap_or_else(|| PathBuf::from("bar-daemon").join(name))
}
