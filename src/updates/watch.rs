use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use notify::{
    EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{AccessKind, AccessMode, CreateKind, ModifyKind, RemoveKind},
};
use shelllist_daemon_core::watch::{DEFAULT_WATCH_LIMIT, WatchBackoff, bounded_watch_paths};
use tokio::{sync::mpsc, time};

#[cfg(test)]
mod tests;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
const COALESCE_WINDOW: Duration = Duration::from_millis(75);

#[derive(Debug, PartialEq, Eq)]
struct Target {
    path: PathBuf,
    device: u64,
    inode: u64,
    recursive: bool,
}

fn target(directory: &Path) -> Option<Target> {
    directory.ancestors().find_map(|path| {
        let metadata = path.metadata().ok()?;
        metadata.is_dir().then(|| Target {
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            recursive: path == directory,
        })
    })
}

// Reads must not invalidate their own snapshot. Close-after-write is useful
// for writers that publish in place; atomic replacements emit rename events.
fn invalidates(event: &notify::Event) -> bool {
    event.need_rescan()
        || !matches!(event.kind, EventKind::Access(_))
        || matches!(
            event.kind,
            EventKind::Access(AccessKind::Close(AccessMode::Write))
        )
}

fn signal(tx: &mpsc::Sender<()>, rebuild: &AtomicBool, result: notify::Result<notify::Event>) {
    match result {
        Ok(event) => {
            if !invalidates(&event) {
                return;
            }
            if event.need_rescan() {
                rebuild.store(true, Ordering::Relaxed);
            }
        }
        Err(error) => {
            tracing::debug!(%error, "NixOS update watch needs recovery");
            rebuild.store(true, Ordering::Relaxed);
        }
    }
    // One pending invalidation is enough. Keep recovery separately so a full
    // queue cannot discard an error/overflow behind an ordinary change.
    let _ = tx.try_send(());
}

struct EventPolicy {
    directory: PathBuf,
    rebuilds_only: bool,
    recursive: bool,
    registered: Arc<[PathBuf]>,
}
impl EventPolicy {
    fn relevant(&self, event: &notify::Event) -> bool {
        !self.rebuilds_only
            || event.need_rescan()
            || event.paths.iter().any(|path| {
                self.directory.starts_with(path)
                    || path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name == "history.jsonl" || name.ends_with(".run.json"))
            })
    }

    fn topology_changed(&self, event: &notify::Event) -> bool {
        self.recursive
            && match event.kind {
                EventKind::Create(CreateKind::Folder)
                | EventKind::Remove(RemoveKind::Folder)
                | EventKind::Any => true,
                EventKind::Modify(ModifyKind::Name(_)) => event
                    .paths
                    .iter()
                    .any(|path| self.registered.contains(path) || path.is_dir()),
                _ => false,
            }
    }

    fn signal(
        &self,
        tx: &mpsc::Sender<()>,
        rebuild: &AtomicBool,
        event: notify::Result<notify::Event>,
    ) {
        if event
            .as_ref()
            .is_ok_and(|event| self.topology_changed(event))
        {
            rebuild.store(true, Ordering::Relaxed);
        }
        if event.as_ref().map_or(true, |event| self.relevant(event)) {
            signal(tx, rebuild, event);
        }
    }
}

pub(super) struct Watch {
    watcher: Option<RecommendedWatcher>,
    target: Option<Target>,
    rebuild: Arc<AtomicBool>,
    tx: mpsc::Sender<()>,
    rx: mpsc::Receiver<()>,
    rebuilds_only: bool,
    recovery: WatchBackoff,
}

impl Watch {
    pub(super) fn new() -> Self {
        let (tx, rx) = mpsc::channel(1);
        Self {
            watcher: None,
            target: None,
            rebuild: Arc::new(AtomicBool::new(false)),
            tx,
            rx,
            rebuilds_only: false,
            recovery: WatchBackoff::new(Instant::now()),
        }
    }

    pub(super) fn new_rebuilds() -> Self {
        Self {
            rebuilds_only: true,
            ..Self::new()
        }
    }

    pub(super) fn refresh(&mut self, directory: &Path) {
        self.refresh_at(directory, Instant::now());
    }

    fn refresh_at(&mut self, directory: &Path, now: Instant) {
        let next = target(directory).map(|mut next| {
            // The manual ledger is flat. Its check-cache can point at enormous
            // Nix closures; watching those trees cannot reveal a ledger change.
            if self.rebuilds_only {
                next.recursive = false;
            }
            next
        });
        let invalidated = self.rebuild.swap(false, Ordering::Relaxed) || self.target != next;
        if invalidated && self.watcher.take().is_some() {
            self.target = None;
            self.recovery.failed(now);
            tracing::debug!(retry_at = ?self.recovery.deadline(), "update watch degraded; polling until recovery");
        }
        if self.watcher.is_some() || !self.recovery.ready(now) {
            return;
        }
        let Some(next) = next else {
            self.recovery.failed(now);
            return;
        };
        match self.install(directory, &next) {
            Ok(watcher) => {
                self.watcher = Some(watcher);
                self.target = Some(next);
                self.recovery.started(now);
            }
            Err(error) => {
                self.recovery.failed(now);
                tracing::warn!(%error, path = %next.path.display(), "NixOS update watcher unavailable; polling continues");
            }
        }
    }

    fn install(&self, directory: &Path, next: &Target) -> anyhow::Result<RecommendedWatcher> {
        let paths: Arc<[PathBuf]> =
            bounded_watch_paths([(next.path.clone(), next.recursive)], DEFAULT_WATCH_LIMIT)?.into();
        let policy = EventPolicy {
            directory: directory.to_owned(),
            rebuilds_only: self.rebuilds_only,
            recursive: next.recursive,
            registered: Arc::clone(&paths),
        };
        let tx = self.tx.clone();
        let rebuild = Arc::clone(&self.rebuild);
        let mut watcher = RecommendedWatcher::new(
            move |event| policy.signal(&tx, &rebuild, event),
            notify::Config::default().with_follow_symlinks(false),
        )?;
        for path in paths.iter() {
            watcher.watch(path, RecursiveMode::NonRecursive)?;
        }
        tracing::debug!(watch_count = paths.len(), "update watcher installed");
        Ok(watcher)
    }

    pub(super) async fn wait(&mut self) {
        let interval = if self.watcher.is_some() {
            RECONCILE_INTERVAL
        } else {
            self.recovery
                .deadline()
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(2))
        };
        if let Ok(Some(())) = time::timeout(interval, self.rx.recv()).await {
            // Fixed rather than trailing-edge debounce: sustained writes must
            // not postpone a snapshot indefinitely. A change during the next
            // read retains a permit for a follow-up snapshot.
            time::sleep(COALESCE_WINDOW).await;
            let _ = self.rx.try_recv();
        }
    }
}
