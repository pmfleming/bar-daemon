//! Same-process replacement windows inherit a verified launch destination.
//! No class matching, process spawning, or replay after owner restart.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tokio::time::Instant;

pub(super) const GRACE: Duration = Duration::from_secs(2);
const MAX_HANDOFFS: usize = 128;
const MAX_REPLACEMENTS: u8 = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Handoff {
    pub window: wire::WindowIdentity,
    pub token: String,
    pub process_start: u64,
    pub excluded: Vec<String>,
    pub replacements: u8,
    pub watching: bool,
    #[serde(skip)]
    pub missing_since: Option<Instant>,
}

fn process_start(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()?
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

impl Handoff {
    pub fn prepare(
        loaded: &Loaded,
        request: &Placement,
        snapshot: &compositor::Snapshot,
    ) -> Option<Self> {
        let reservation = &loaded.journal.reservations[&request.lease.request_id];
        // Only fresh launch-created destinations. Existing singleton windows and
        // standard/manual destinations retain their current routing semantics.
        if reservation.request.workspace_id.is_some()
            || reservation.request.purpose != Purpose::Launch
            || loaded.journal.handoffs.len() >= MAX_HANDOFFS
        {
            return None;
        }
        let excluded: Vec<_> = snapshot
            .windows
            .iter()
            .filter(|w| w.pid == request.window.pid && w.address != request.window.address)
            .take(513)
            .map(|w| w.address.clone())
            .collect();
        if excluded.len() > 512 {
            return None;
        }
        Some(Self {
            window: wire::WindowIdentity {
                workspace_id: request.lease.workspace_id,
                ..request.window.clone()
            },
            token: format!(
                "handoff-{}",
                crate::state::publication::Publication::new().epoch
            ),
            process_start: process_start(request.window.pid)?,
            excluded,
            replacements: 0,
            watching: false,
            missing_since: None,
        })
    }
}

impl Service {
    pub(super) fn wake_after_grace(&self) {
        let changed = self.changed.clone();
        tokio::spawn(async move {
            tokio::time::sleep(GRACE).await;
            changed.notify_one();
        });
    }

    /// Report whether placement changed the snapshot. Unknown
    /// observations abort cleanup; they are not permission to retire or move.
    pub(super) async fn reconcile_handoffs(
        &self,
        loaded: &mut Loaded,
        snapshot: &compositor::Snapshot,
    ) -> Result<bool> {
        let keys: Vec<_> = loaded.journal.handoffs.keys().cloned().collect();
        let events = std::mem::take(&mut *self.handoff_events.lock().unwrap());
        // Fence all affected observers before any IPC can fail. Draining the
        // event batch must not lose cancellation for later observers on error.
        let mut fenced = false;
        for h in loaded.journal.handoffs.values_mut() {
            if h.watching
                && (events.lost
                    || events.moved.iter().any(|(address, workspace)| {
                        *workspace != h.window.workspace_id
                            && (address == &h.window.address
                                || snapshot
                                    .windows
                                    .iter()
                                    .any(|w| &w.address == address && w.pid == h.window.pid))
                    }))
            {
                h.watching = false;
                fenced = true;
            }
        }
        if fenced {
            loaded.save().await?;
        }
        let mut moved = false;
        for key in keys {
            let mut handoff = loaded.journal.handoffs[&key].clone();
            let id = handoff.window.workspace_id;
            let reservation = &loaded.journal.reservations[&key];
            let entry = loaded.journal.entries.get(&id);
            if !handoff.watching
                || events.lost
                || events.moved.iter().any(|(address, workspace)| {
                    address == &handoff.window.address && *workspace != id
                })
                || process_start(handoff.window.pid) != Some(handoff.process_start)
                || entry.is_none_or(|e| e.generation != reservation.lease.generation)
                || loaded
                    .journal
                    .closes
                    .values()
                    .any(|c| c.request.context.workspace_id == id)
            {
                self.cancel_handoff(loaded, &key).await?;
                continue;
            }
            match compositor::pinned_state(
                &self.client,
                &handoff.token,
                &handoff.window.address,
                handoff.window.pid,
                id,
            )
            .await?
            {
                compositor::PinnedState::Present => {
                    // Windows observed alongside the predecessor are not its
                    // replacement, even if they outlive it (for example dialogs).
                    for w in snapshot.windows.iter().filter(|w| {
                        w.pid == handoff.window.pid && w.address != handoff.window.address
                    }) {
                        if !handoff.excluded.contains(&w.address) {
                            handoff.excluded.push(w.address.clone());
                        }
                    }
                    if handoff.excluded.len() > 512 {
                        self.cancel_handoff(loaded, &key).await?;
                    } else {
                        handoff.missing_since = None;
                        loaded.journal.handoffs.insert(key, handoff);
                    }
                    continue;
                }
                compositor::PinnedState::Moved | compositor::PinnedState::Lost => {
                    self.cancel_handoff(loaded, &key).await?;
                    continue;
                }
                compositor::PinnedState::Gone => {}
            }
            let since = *handoff.missing_since.get_or_insert_with(|| {
                self.wake_after_grace();
                Instant::now()
            });
            loaded.journal.handoffs.get_mut(&key).unwrap().missing_since = Some(since);
            if since.elapsed() >= GRACE || handoff.replacements >= MAX_REPLACEMENTS {
                self.cancel_handoff(loaded, &key).await?;
                continue;
            }
            let candidates: Vec<_> = snapshot
                .windows
                .iter()
                .filter(|w| {
                    w.mapped
                        && w.pid == handoff.window.pid
                        && w.workspace.id > 0
                        && !handoff.excluded.contains(&w.address)
                })
                .collect();
            if candidates.is_empty() {
                loaded.journal.handoffs.insert(key, handoff);
                continue;
            }
            if candidates.len() != 1 {
                // Multiple new windows are ambiguous. Never guess by class/title.
                self.cancel_handoff(loaded, &key).await?;
                continue;
            }
            let candidate = candidates[0];
            if events
                .moved
                .iter()
                .any(|(address, workspace)| address == &candidate.address && *workspace != id)
            {
                self.cancel_handoff(loaded, &key).await?;
                continue;
            }
            let monitor = &entry.unwrap().monitor;
            let expression = compositor::replace_pinned(
                &handoff.token,
                &handoff.window.address,
                candidate,
                id,
                monitor,
            )?;
            // Durable admission before dispatch. A crash or uncertain reply
            // fences this observer, rather than replaying a window move.
            loaded.journal.handoffs.get_mut(&key).unwrap().watching = false;
            loaded.save().await?;
            if compositor::execute(&self.client, &expression)
                .await
                .is_err()
            {
                self.cancel_handoff(loaded, &key).await?;
                continue;
            }
            handoff.window.address = candidate.address.clone();
            handoff.replacements += 1;
            handoff.missing_since = None;
            loaded.journal.handoffs.insert(key, handoff);
            loaded.save().await?;
            moved = true;
        }
        Ok(moved)
    }

    pub(super) async fn cancel_handoff(&self, loaded: &mut Loaded, key: &str) -> Result<()> {
        if let Some(handoff) = loaded.journal.handoffs.get_mut(key) {
            handoff.watching = false;
            let token = handoff.token.clone();
            loaded.save().await?;
            compositor::execute(&self.client, &compositor::release_window(&token)?).await?;
            loaded.journal.handoffs.remove(key);
            loaded.save().await?;
        }
        Ok(())
    }
}

pub(super) type EmptySince = BTreeMap<i64, Instant>;

#[derive(Default)]
pub(super) struct Events {
    moved: std::collections::BTreeSet<(String, i64)>,
    lost: bool,
}

impl Service {
    /// Synchronous bounded intake: never block compositor event intake on IPC.
    pub(crate) fn handoff_event(&self, event: &str) {
        let mut events = self.handoff_events.lock().unwrap();
        if event == "disconnected" || event.starts_with("configreloaded>>") {
            events.lost = true;
        } else if let Some(value) = event.strip_prefix("movewindowv2>>") {
            let mut fields = value.split(',');
            if let (Some(address), Some(workspace)) = (fields.next(), fields.next())
                && let Ok(workspace) = workspace.parse()
            {
                if events.moved.len() < 512 {
                    events
                        .moved
                        .insert((format!("0x{}", address.trim_start_matches("0x")), workspace));
                } else {
                    events.lost = true;
                }
            }
        }
    }
}
