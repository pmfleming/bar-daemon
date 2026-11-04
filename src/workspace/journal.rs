use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use shelllist_daemon_core::{
    AtomicFilePolicy, read_bytes_bounded,
    workspace_wire::{self as wire, Create, Lease, WindowIdentity},
};
use std::{collections::BTreeMap, path::Path};

pub(super) const MAX_RECORDS: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Entry {
    pub id: i64,
    pub generation: String,
    pub monitor: String,
    pub occupied: bool,
    pub landing_pad: bool,
    #[serde(default)]
    pub keep_until_closed: bool,
    pub origin: String,
    pub icon: String,
    pub cue_assigned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(super) struct Reservation {
    pub request: Create,
    pub lease: Lease,
    pub writer: String,
    pub released: bool,
    pub placed: bool,
    pub placement: Option<wire::Placement>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(super) struct Target {
    pub window: WindowIdentity,
    pub token: String,
    pub attempted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(super) struct CloseRecord {
    pub request: wire::Close,
    pub targets: Vec<Target>,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Journal {
    pub version: u32,
    pub session: String,
    pub high: i64,
    pub entries: BTreeMap<i64, Entry>,
    pub reservations: BTreeMap<String, Reservation>,
    pub closes: BTreeMap<String, CloseRecord>,
    pub history: BTreeMap<String, Vec<i64>>,
    #[serde(default)]
    pub handoffs: BTreeMap<String, super::handoff::Handoff>,
}

impl Journal {
    pub fn empty(session: String) -> Self {
        Self {
            version: wire::VERSION,
            session,
            high: 5,
            entries: BTreeMap::new(),
            reservations: BTreeMap::new(),
            closes: BTreeMap::new(),
            history: BTreeMap::new(),
            handoffs: BTreeMap::new(),
        }
    }

    pub fn load(path: &Path, session: &str) -> Result<Self> {
        let bytes = match read_bytes_bounded(path, 16 * 1024 * 1024) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(Self::empty(session.into())),
            Err(error) => return Err(error.into()),
        };
        let mut journal: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            journal.version == wire::VERSION && journal.session == session,
            "workspace journal session/version mismatch"
        );
        ensure!(
            (5..=wire::MAX_WORKSPACE_ID).contains(&journal.high),
            "invalid workspace high watermark"
        );
        ensure!(
            journal.reservations.len() + journal.closes.len() <= MAX_RECORDS,
            "workspace journal exceeds admission limit"
        );
        ensure!(
            journal
                .entries
                .iter()
                .all(|(id, entry)| *id > 5 && *id <= journal.high && entry.id == *id),
            "invalid workspace entries"
        );
        ensure!(
            journal
                .reservations
                .iter()
                .all(|(key, r)| wire::valid_request_id(key)
                    && key == &r.request.request_id
                    && key == &r.lease.request_id
                    && r.lease.session == session
                    && r.request.context.session == session
                    && (1..=wire::MAX_WORKSPACE_ID).contains(&r.lease.workspace_id)),
            "invalid reservation identity"
        );
        // Older journals already identify manual creates by their reservation.
        // Preserve those still-owned entries, but never undo an admitted close.
        for entry in journal.entries.values_mut() {
            if !journal
                .closes
                .values()
                .any(|close| close.request.context.workspace_id == entry.id)
                && journal.reservations.values().any(|reservation| {
                    reservation.request.purpose == wire::Purpose::LandingPad
                        && reservation.request.workspace_id.is_none()
                        && reservation.lease.workspace_id == entry.id
                        && reservation.lease.generation == entry.generation
                })
            {
                entry.keep_until_closed = true;
            }
        }
        // A restarted owner fences every old placement writer. No old effect is
        // resumed, even when its last durable receipt precedes compositor dispatch.
        for reservation in journal.reservations.values_mut() {
            if !reservation.released {
                reservation.released = true;
                reservation.status = "unknown".into();
            }
        }
        for close in journal.closes.values_mut() {
            if matches!(close.status.as_str(), "dispatching" | "waiting") {
                close.status = "unknown".into();
            }
        }
        ensure!(journal.handoffs.len() <= 128, "too many launch handoffs");
        for (key, handoff) in &mut journal.handoffs {
            ensure!(
                journal.reservations.contains_key(key),
                "orphan launch handoff"
            );
            ensure!(
                handoff.window.workspace_id == journal.reservations[key].lease.workspace_id
                    && handoff.excluded.len() <= 512
                    && wire::valid_request_id(&handoff.token),
                "invalid launch handoff"
            );
            // A replacement is never replayed or re-adopted after restart.
            handoff.watching = false;
        }
        Ok(journal)
    }

    pub async fn save(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec(self)?;
        ensure!(
            bytes.len() <= 16 * 1024 * 1024,
            "workspace journal exceeds byte limit"
        );
        shelllist_daemon_tokio::write_bytes_atomic_async(
            path.to_owned(),
            bytes,
            AtomicFilePolicy::PRIVATE,
        )
        .await?;
        Ok(())
    }

    pub fn admit(&self, key: &str) -> Result<()> {
        ensure!(wire::valid_request_id(key), "invalid request key");
        // Keep tombstones for this session. At the bound reject admission instead
        // of forgetting a key and risking replay; live desktop use continues.
        ensure!(
            self.reservations.len() + self.closes.len() < MAX_RECORDS,
            "workspace session request limit reached"
        );
        ensure!(
            !self.reservations.contains_key(key) && !self.closes.contains_key(key),
            "request key already used"
        );
        Ok(())
    }

    pub fn allocate(&mut self, live_ids: impl Iterator<Item = i64>) -> Result<i64> {
        let highest = live_ids.max().unwrap_or(5).max(self.high).max(5);
        ensure!(
            highest < wire::MAX_WORKSPACE_ID,
            "workspace ID space exhausted"
        );
        self.high = highest + 1;
        Ok(self.high)
    }

    pub fn leased(&self, id: i64) -> bool {
        self.reservations
            .values()
            .any(|r| r.lease.workspace_id == id && !r.released)
    }

    pub fn closing(&self, id: i64) -> bool {
        self.closes.values().any(|r| {
            r.request.context.workspace_id == id
                && matches!(r.status.as_str(), "dispatching" | "waiting")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn allocation_is_monotonic_durable_bounded_and_corruption_is_not_empty() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("journal.json");
        let mut journal = Journal::load(&path, "s").unwrap();
        assert_eq!(journal.allocate([1, 9, -99].into_iter()).unwrap(), 10);
        journal.save(&path).await.unwrap();
        let mut restored = Journal::load(&path, "s").unwrap();
        assert_eq!(restored.allocate([1, 6].into_iter()).unwrap(), 11);
        assert!(Journal::load(&path, "different").is_err());
        restored.high = wire::MAX_WORKSPACE_ID;
        assert!(restored.allocate([].into_iter()).is_err());
        std::fs::write(path.clone(), b"broken").unwrap();
        assert!(Journal::load(&path, "s").is_err());
    }
}
