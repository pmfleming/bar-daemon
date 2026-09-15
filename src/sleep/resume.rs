//! Shared detection only; sleep, lock, and inhibitor policy stays in bar-daemon.
pub(super) use shelllist_daemon_tokio::{ResumeDetector, suspend_offset};
