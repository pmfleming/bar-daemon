//! One admission budget for blocking catalog/page/log reads across all callers.
use anyhow::{Context, Result};
use tokio::sync::Semaphore;

/// One deadline covers admission and execution. Running blocking work keeps its
/// permit even after this future times out; it is never replayed.
pub(super) async fn bounded<T: Send + 'static>(
    timeout_message: &'static str,
    read: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::time::timeout(std::time::Duration::from_secs(3), run(read))
        .await
        .context(timeout_message)?
}

pub(super) async fn run<T: Send + 'static>(
    read: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    static SLOTS: Semaphore = Semaphore::const_new(2);
    with_limit(&SLOTS, read).await
}

async fn with_limit<T: Send + 'static>(
    slots: &'static Semaphore,
    read: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let permit = slots.acquire().await?;
    tokio::task::spawn_blocking(move || {
        // Dropping the awaiting future must not admit replacement work while this
        // non-cancellable filesystem operation is still running.
        let _permit = permit;
        read()
    })
    .await
    .context("Join update reader")?
}

#[cfg(test)]
mod tests {
    use super::with_limit;
    use anyhow::Result;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use tokio::sync::{Semaphore, oneshot};

    #[tokio::test]
    async fn cancellation_keeps_running_work_admitted_but_does_not_start_cancelled_waiters() {
        static SLOTS: Semaphore = Semaphore::const_new(1);
        let (started, ready) = oneshot::channel();
        let (release, wait) = mpsc::channel();
        let first = tokio::spawn(with_limit(&SLOTS, move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
            Ok(())
        }));
        ready.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(SLOTS.available_permits(), 0);
        let ran = Arc::new(AtomicBool::new(false));
        {
            let ran = Arc::clone(&ran);
            let cancelled = with_limit(&SLOTS, move || {
                ran.store(true, Ordering::SeqCst);
                Ok(())
            });
            tokio::pin!(cancelled);
            assert!(futures::poll!(&mut cancelled).is_pending());
        }
        let next = with_limit(&SLOTS, || Ok(42));
        tokio::pin!(next);
        assert!(futures::poll!(&mut next).is_pending());
        release.send(()).unwrap();
        assert_eq!(next.await.unwrap(), 42);
        assert!(!ran.load(Ordering::SeqCst));
        assert_eq!(SLOTS.available_permits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_returns_without_replaying_or_cancelling_running_filesystem_work() {
        let (started, ready) = oneshot::channel();
        let (release, wait) = mpsc::channel();
        let (finished, mut completed) = oneshot::channel();
        let task = tokio::spawn(super::bounded("fixture deadline", move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
            finished.send(()).unwrap();
            Ok(())
        }));
        ready.await.unwrap();
        tokio::time::advance(std::time::Duration::from_secs(3)).await;
        assert_eq!(
            task.await.unwrap().unwrap_err().to_string(),
            "fixture deadline"
        );
        assert!(matches!(
            completed.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        release.send(()).unwrap();
        completed.await.unwrap();
    }

    #[tokio::test]
    async fn failures_and_panics_release_admission() {
        static SLOTS: Semaphore = Semaphore::const_new(1);
        assert!(
            with_limit(&SLOTS, || -> Result<()> {
                anyhow::bail!("fixture failure")
            })
            .await
            .is_err()
        );
        assert!(
            with_limit(&SLOTS, || -> Result<()> { panic!("fixture panic") })
                .await
                .is_err()
        );
        assert_eq!(SLOTS.available_permits(), 1);
        assert_eq!(with_limit(&SLOTS, || Ok(42)).await.unwrap(), 42);
    }
}
