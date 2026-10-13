//! Interactive scope is opt-in. Background policy never opens a polkit prompt.
use std::time::Duration;
use tokio::sync::watch;

pub(crate) const AUTHORIZATION_BUDGET: Duration = Duration::from_secs(90);
pub(crate) const HELPER_BUDGET: Duration = Duration::from_secs(100);

tokio::task_local! {
    pub(crate) static INTERACTIVE: watch::Receiver<bool>;
}

pub(crate) fn cancellation() -> Option<watch::Receiver<bool>> {
    INTERACTIVE.try_with(Clone::clone).ok()
}

#[derive(Debug)]
pub(crate) struct OutcomeUnknown;
impl std::fmt::Display for OutcomeUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("battery write outcome unknown; reconcile hardware state before another action")
    }
}
impl std::error::Error for OutcomeUnknown {}

pub(crate) fn is_unknown(error: &anyhow::Error) -> bool {
    error.downcast_ref::<OutcomeUnknown>().is_some()
}

pub(crate) async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    while !*receiver.borrow_and_update() {
        if receiver.changed().await.is_err() {
            return;
        }
    }
}
