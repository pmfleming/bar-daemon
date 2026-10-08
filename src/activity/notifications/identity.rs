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
        if let Some((id, text)) = read_desktop(root, &path) {
            icons.insert(id, &text);
        }
    }
}
fn read_desktop(root: &Path, path: &Path) -> Option<(String, String)> {
    if path.extension()? != "desktop" || path.metadata().ok()?.len() > 65536 {
        return None;
    }
    let id = path
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('/', "-");
    let id = id.trim_end_matches(".desktop");
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(65537)
        .read_to_string(&mut text)
        .ok()?;
    (id.len() <= 1024 && text.len() <= 65536).then(|| (id.to_owned(), text))
}
impl Icons {
    fn insert(&mut self, id: String, text: &str) {
        let mut section = false;
        let (mut name, mut icon) = ("", "");
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line == "[Desktop Entry]";
            }
            match (section, line.split_once('=')) {
                (true, Some(("Name", value))) => name = value,
                (true, Some(("Icon", value))) => icon = value,
                _ => {}
            }
        }
        if icon.is_empty() || icon.len() > 1024 || icon.contains("://") {
            return;
        }
        self.ids.entry(id).or_insert_with(|| icon.into());
        if !name.is_empty() && name.len() <= 1024 {
            self.names
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

// Descriptive grouping, not authentication. Never route a mutation by app key.
// Artwork is not sender identity; unnamed senders must remain distinct.
pub(super) fn app_key(desktop: &str, name: &str, id: u32, created: u64) -> String {
    if desktop.len() > 1024 {
        format!("unknown:{created}:{id}")
    } else if !desktop.trim().is_empty() {
        format!("desktop:{}", desktop.trim())
    } else if name.len() > 1024 || name.trim().is_empty() {
        format!("unknown:{created}:{id}")
    } else {
        format!("named:{}", name.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::{Icons, app_key, collect};
    #[test]
    fn desktop_identity_is_bounded_and_ignores_action_icons_and_remote_artwork() {
        assert_eq!(app_key(&"d".repeat(1025), "App", 7, 8), "unknown:8:7");
        assert_ne!(app_key("", "a:b", 1, 2), app_key("", "a", 1, 2));
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
