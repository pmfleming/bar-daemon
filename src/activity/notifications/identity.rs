//! Capture desktop application identity independently of notification artwork.
//! Theme names (not transient notification images) survive history/restarts.
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::LazyLock,
};

#[derive(Default)]
struct Icons {
    ids: BTreeMap<String, String>,
    names: BTreeMap<String, String>,
    scanned: usize,
}
static ICONS: LazyLock<Icons> = LazyLock::new(|| {
    let mut result = Icons::default();
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")));
    let dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    for base in home.into_iter().chain(std::env::split_paths(&dirs)) {
        let root = base.join("applications");
        collect(&root, &root, 0, &mut result);
    }
    result
});
fn collect(root: &Path, dir: &Path, depth: usize, icons: &mut Icons) {
    if depth > 3 || icons.ids.len() >= 4096 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten().take(4096) {
        if icons.scanned >= 16384 || icons.ids.len() >= 4096 {
            break;
        }
        icons.scanned += 1;
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, depth + 1, icons);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "desktop")
            || path.metadata().map_or(true, |m| m.len() > 65536)
        {
            continue;
        }
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut text = String::new();
        if file.take(65537).read_to_string(&mut text).is_err() || text.len() > 65536 {
            continue;
        }
        let mut section = false;
        let mut name = "";
        let mut icon = "";
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line == "[Desktop Entry]";
            }
            if !section {
                continue;
            }
            if let Some(value) = line.strip_prefix("Name=") {
                name = value;
            }
            if let Some(value) = line.strip_prefix("Icon=") {
                icon = value;
            }
        }
        if icon.is_empty() || icon.len() > 1024 || icon.contains("://") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let id = relative
            .to_string_lossy()
            .replace('/', "-")
            .trim_end_matches(".desktop")
            .to_owned();
        if id.len() > 1024 {
            continue;
        }
        icons.ids.entry(id).or_insert_with(|| icon.into());
        if !name.is_empty() && name.len() <= 1024 {
            icons
                .names
                .entry(name.to_lowercase())
                .or_insert_with(|| icon.into());
        }
    }
}
pub(crate) fn preload() {
    LazyLock::force(&ICONS);
}
pub(crate) fn icon(desktop: &str, name: &str) -> String {
    ICONS
        .ids
        .get(desktop.trim().trim_end_matches(".desktop"))
        .or_else(|| ICONS.names.get(&name.trim().to_lowercase()))
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_identity_is_bounded_and_ignores_action_icons_and_remote_artwork() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("org.example.App.desktop"), "[Desktop Entry]\nName=Example\nIcon=example-app\n[Desktop Action Capture]\nIcon=screenshot-art\n").unwrap();
        std::fs::write(
            directory.path().join("remote.desktop"),
            "[Desktop Entry]\nName=Remote\nIcon=https://example.org/icon.png\n",
        )
        .unwrap();
        std::fs::write(directory.path().join("large.desktop"), "x".repeat(65537)).unwrap();
        let mut icons = Icons::default();
        collect(directory.path(), directory.path(), 0, &mut icons);
        assert_eq!(icons.ids.len(), 1);
        assert_eq!(icons.ids["org.example.App"], "example-app");
        assert_eq!(icons.names["example"], "example-app");
        let other = tempfile::tempdir().unwrap();
        std::fs::write(
            other.path().join("org.example.App.desktop"),
            "[Desktop Entry]\nName=Example\nIcon=lower-priority\n",
        )
        .unwrap();
        collect(other.path(), other.path(), 0, &mut icons);
        assert_eq!(
            icons.ids["org.example.App"], "example-app",
            "XDG directory precedence is stable"
        );
    }
}
