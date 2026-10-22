//! Daemon-owned toast ordering and bounded reading leases. UI only renders decks.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::model::ActiveNotification;

pub(super) const LEASE_MS: u64 = 3_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToastDeck {
    pub monitor: String,
    pub key: String,
    pub records: Vec<ActiveNotification>,
}

#[derive(Default)]
pub(super) struct Decks {
    leases: BTreeMap<String, (u64, u64)>,
    published: Vec<ToastDeck>,
}

impl Decks {
    pub(super) fn reading(&self, monitor: &str) -> bool {
        self.leases.contains_key(monitor)
    }

    pub(super) fn deadline(&self) -> Option<u64> {
        self.leases.values().map(|(_, deadline)| *deadline).min()
    }

    pub(super) fn set_reading(
        &mut self,
        monitor: String,
        reading: bool,
        now: u64,
        active: &mut BTreeMap<u32, ActiveNotification>,
    ) {
        if reading {
            let lease = self.leases.entry(monitor).or_insert((now, now));
            lease.1 = now.saturating_add(LEASE_MS);
        } else if let Some((started, _)) = self.leases.remove(&monitor) {
            for n in active.values_mut().filter(|n| n.source_monitor == monitor) {
                if let Some(deadline) = &mut n.toast_expires_unix_ms {
                    *deadline =
                        deadline.saturating_add(now.saturating_sub(started.max(n.updated_unix_ms)));
                }
            }
        }
    }

    pub(super) fn expire(&mut self, now: u64, active: &mut BTreeMap<u32, ActiveNotification>) {
        let expired: Vec<_> = self
            .leases
            .iter()
            .filter(|(_, (_, deadline))| *deadline <= now)
            .map(|(monitor, _)| monitor.clone())
            .collect();
        for monitor in expired {
            self.set_reading(monitor, false, now, active);
        }
    }

    pub(super) fn project(&mut self, records: &[ActiveNotification], dnd: bool) -> Vec<ToastDeck> {
        let eligible = |n: &&ActiveNotification| n.toast_visible && (!dnd || n.dnd_bypass);
        // Reading freezes *all* decks on that monitor, not just the hovered app.
        // Acknowledged removals still compact; no arrival/replacement can steal a slot.
        let mut next: Vec<_> = self
            .published
            .iter()
            .filter(|d| self.reading(&d.monitor))
            .filter_map(|deck| {
                let records: Vec<_> = deck
                    .records
                    .iter()
                    .filter_map(|old| {
                        records
                            .iter()
                            .filter(eligible)
                            .find(|n| {
                                n.id == old.id
                                    && n.created_unix_ms == old.created_unix_ms
                                    && n.updated_unix_ms == old.updated_unix_ms
                            })
                            .cloned()
                    })
                    .collect();
                (!records.is_empty()).then(|| ToastDeck {
                    records,
                    ..deck.clone()
                })
            })
            .collect();
        let mut ordered: Vec<_> = records
            .iter()
            .filter(eligible)
            .filter(|n| !self.reading(&n.source_monitor))
            .collect();
        ordered.sort_by_key(|n| std::cmp::Reverse((n.created_unix_ms, n.id)));
        for n in ordered {
            let key = n.app_key();
            if let Some(deck) = next
                .iter_mut()
                .find(|d| d.monitor == n.source_monitor && d.key == key)
            {
                deck.records.push(n.clone());
            } else if next
                .iter()
                .filter(|d| d.monitor == n.source_monitor)
                .count()
                < 3
            {
                next.push(ToastDeck {
                    monitor: n.source_monitor.clone(),
                    key,
                    records: vec![n.clone()],
                });
            }
        }
        self.published.clone_from(&next);
        next
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::incoming;
    use super::*;

    fn record(id: u32, app: &str) -> ActiveNotification {
        let mut n =
            ActiveNotification::from_incoming(id, incoming(app, "message", ""), u64::from(id));
        n.source_monitor = "DP-1".into();
        n.toast_expires_unix_ms = Some(5_000);
        n
    }

    #[test]
    fn reading_freezes_all_apps_but_acknowledged_dismissal_compacts() {
        let mut decks = Decks::default();
        let mut active = BTreeMap::from([(1, record(1, "chat")), (2, record(2, "chat"))]);
        let project = |decks: &mut Decks, active: &BTreeMap<_, _>| {
            decks.project(&active.values().cloned().collect::<Vec<_>>(), false)
        };
        assert_eq!(project(&mut decks, &active)[0].records[0].id, 2);
        decks.set_reading("DP-1".into(), true, 100, &mut active);
        active.insert(3, record(3, "browser"));
        active.insert(4, record(4, "chat"));
        let frozen = project(&mut decks, &active);
        assert_eq!(frozen.len(), 1);
        assert_eq!(frozen[0].records.len(), 2);
        active.remove(&2);
        assert_eq!(project(&mut decks, &active)[0].records[0].id, 1);
        decks.expire(3_100, &mut active);
        assert!(!decks.reading("DP-1"));
        let released = project(&mut decks, &active);
        assert_eq!(released.len(), 2);
        assert_eq!(released[0].records[0].id, 4);
        assert_eq!(active[&1].toast_expires_unix_ms, Some(8_000));
    }

    #[test]
    fn lease_renewal_does_not_extend_protocol_expiry_or_reveal_replacements() {
        let mut decks = Decks::default();
        let mut n = record(1, "chat");
        n.expires_unix_ms = Some(500);
        let mut active = BTreeMap::from([(1, n.clone())]);
        decks.project(&[n], false);
        decks.set_reading("DP-1".into(), true, 100, &mut active);
        decks.set_reading("DP-1".into(), true, 200, &mut active);
        active.get_mut(&1).unwrap().updated_unix_ms = 201;
        assert!(
            decks
                .project(&active.values().cloned().collect::<Vec<_>>(), false)
                .is_empty()
        );
        decks.expire(3_100, &mut active);
        assert!(decks.reading("DP-1"));
        decks.expire(3_200, &mut active);
        assert_eq!(active[&1].expires_unix_ms, Some(500));
    }
}
