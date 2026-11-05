use super::journal::Journal;
use serde::{Deserialize, Serialize};
use shelllist_hyprland::workspace::Snapshot;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct View {
    pub version: u32,
    pub session: String,
    pub available: bool,
    pub monitors: BTreeMap<String, Vec<Item>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Item {
    pub id: i64,
    pub label: String,
    pub icon: String,
    pub number: String,
    pub temporary: bool,
    pub divider_before: bool,
    pub active: bool,
    pub occupied: bool,
    pub urgent: bool,
    pub accessible_name: String,
}

const CATEGORIES: [(&str, &str); 5] = [
    ("Shell", "terminal"),
    ("Browser", "language"),
    ("Code", "code"),
    ("Media", "music_note"),
    ("Text", "description"),
];

/// The bar and positional commands share one ordering, including retained empties.
pub(super) fn workspace_ids<'a>(
    journal: &Journal,
    monitor: &str,
    workspaces: impl IntoIterator<Item = (i64, &'a str)>,
) -> Vec<i64> {
    let live: BTreeMap<_, _> = workspaces.into_iter().collect();
    let mut ids = vec![1, 2, 3, 4, 5];
    ids.extend(
        live.iter()
            .filter(|(id, owner)| **id > 5 && **owner == monitor)
            .map(|(id, _)| *id),
    );
    ids.extend(
        journal
            .entries
            .values()
            .filter(|entry| live.get(&entry.id).copied().unwrap_or(&entry.monitor) == monitor)
            .map(|entry| entry.id),
    );
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub(super) fn view(journal: &Journal, state: &crate::model::WorkspaceState) -> View {
    let mut view = View {
        version: shelllist_daemon_core::workspace_wire::VERSION,
        session: journal.session.clone(),
        available: state.available,
        ..Default::default()
    };
    for monitor in &state.monitors {
        let ids = workspace_ids(
            journal,
            &monitor.name,
            state.workspaces.iter().map(|w| (w.id, w.monitor.as_str())),
        );
        let first_extra = ids.iter().copied().find(|id| *id > 5);
        let items = ids.into_iter().map(|id| {
            let workspace = state.workspaces.iter().find(|w| w.id == id);
            let entry = journal.entries.get(&id);
            let (label, icon) = if (1..=5).contains(&id) {
                let (label, icon) = CATEGORIES[id as usize-1];
                (label.to_owned(), icon.to_owned())
            } else { entry.map(|e| (e.origin.clone(), e.icon.clone())).unwrap_or_else(|| ("Workspace".into(), "workspace".into())) };
            let count = workspace.map_or(0, |w| w.windows);
            Item { id, accessible_name:format!("Workspace {id}, {label}, {count} windows. Shift+Super+Q closes its windows."), label, icon, number:if id>5 {id.to_string()} else {String::new()}, temporary:id>5, divider_before:Some(id)==first_extra, active:monitor.active_workspace_id==id, occupied:count>0, urgent:workspace.is_some_and(|w| w.urgent) }
        }).collect();
        view.monitors.insert(monitor.name.clone(), items);
    }
    view
}

pub(super) fn fallback(
    journal: &Journal,
    snapshot: &Snapshot,
    monitor: &str,
    retiring: i64,
) -> Option<i64> {
    let usable = |id: i64| {
        id != retiring
            && id > 0
            && !journal.closing(id)
            && snapshot
                .workspaces
                .iter()
                .any(|w| w.id == id && w.monitor == monitor)
    };
    journal
        .history
        .get(monitor)
        .into_iter()
        .flatten()
        .copied()
        .find(|id| usable(*id))
        .or_else(|| {
            snapshot
                .workspaces
                .iter()
                .map(|w| w.id)
                .find(|id| usable(*id))
        })
        .or_else(|| {
            (1..=5).find(|id| {
                !journal.closing(*id) && !snapshot.workspaces.iter().any(|w| w.id == *id)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_never_pulls_foreign_standard_or_reuses_retiring_workspace() {
        let mut journal = Journal::empty("s".into());
        let mut snapshot = Snapshot {
            session: "s".into(),
            monitors: vec![],
            windows: vec![],
            workspaces: (1..=5)
                .map(|id| shelllist_hyprland::workspace::Workspace {
                    id,
                    monitor: "other".into(),
                    windows: 1,
                })
                .collect(),
        };
        assert_eq!(fallback(&journal, &snapshot, "local", 6), None);
        snapshot
            .workspaces
            .push(shelllist_hyprland::workspace::Workspace {
                id: 7,
                monitor: "local".into(),
                windows: 1,
            });
        assert_eq!(fallback(&journal, &snapshot, "local", 6), Some(7));
        assert_eq!(fallback(&journal, &snapshot, "local", 7), None);
        snapshot.workspaces.remove(0);
        assert_eq!(fallback(&journal, &snapshot, "local", 7), Some(1));
        snapshot
            .workspaces
            .push(shelllist_hyprland::workspace::Workspace {
                id: 8,
                monitor: "local".into(),
                windows: 1,
            });
        journal.history.insert("local".into(), vec![6, 2, 8, 7]);
        assert_eq!(fallback(&journal, &snapshot, "local", 6), Some(8));
    }
}
