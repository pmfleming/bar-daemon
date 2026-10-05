use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use notify::{
    EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{AccessKind, AccessMode},
};
use tokio::{sync::mpsc, time};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
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

pub(super) struct Watch {
    watcher: Option<RecommendedWatcher>,
    target: Option<Target>,
    rebuild: Arc<AtomicBool>,
    tx: mpsc::Sender<()>,
    rx: mpsc::Receiver<()>,
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
        }
    }

    pub(super) fn refresh(&mut self, directory: &Path) {
        let next = target(directory);
        if self.rebuild.swap(false, Ordering::Relaxed) || self.target != next {
            // A pathname can refer to a new inode after removal/recreation.
            // Dropping the old watcher also clears stale recursive watches.
            self.watcher = None;
            self.target = None;
        }
        if self.watcher.is_some() {
            return;
        }
        let Some(next) = next else { return };
        let tx = self.tx.clone();
        let rebuild = self.rebuild.clone();
        let result = notify::recommended_watcher(move |event| signal(&tx, &rebuild, event))
            .and_then(|mut watcher| {
                watcher.watch(
                    &next.path,
                    if next.recursive {
                        RecursiveMode::Recursive
                    } else {
                        RecursiveMode::NonRecursive
                    },
                )?;
                Ok(watcher)
            });
        match result {
            Ok(watcher) => {
                self.watcher = Some(watcher);
                self.target = Some(next);
            }
            Err(error) => {
                tracing::debug!(%error, path = %next.path.display(), "NixOS update watcher unavailable");
            }
        }
    }

    pub(super) async fn wait(&mut self) {
        let interval = if self.watcher.is_some() {
            RECONCILE_INTERVAL
        } else {
            RETRY_INTERVAL
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

#[cfg(test)]
mod tests;
