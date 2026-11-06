//! Read-only framework observation; power policy stays in its owner.
use crate::state::StateStore;
use shelllist_daemon_tokio::{SleepObservation, monitor_sleep_transitions};

pub(crate) async fn monitor(store: StateStore) {
    let (sender, mut changes) = tokio::sync::watch::channel(SleepObservation::default());
    let observe = async {
        let mut generation = 0;
        while changes.changed().await.is_ok() {
            let state = *changes.borrow_and_update();
            if state.resume_generation != generation {
                generation = state.resume_generation;
                store.record_resume().await;
            }
            // A coalesced watch may already describe the next preparation.
            store.record_sleep_preparation(state.preparing).await;
        }
    };
    tokio::select! { _ = monitor_sleep_transitions(sender) => {}, _ = observe => {} }
}
