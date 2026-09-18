//! A temporary, daemon-owned logind inhibitor. Never inhibit idle: automatic
//! locking and DPMS must continue while sleep is blocked.
use super::*;

const WHO: &str = "Shelllist Keep awake";
const WHY: &str = "Keep awake enabled in Battery & Power";
// High-level sleep alone is insufficient: logind normally ignores it for lid
// close (LidSwitchIgnoreInhibited=yes). Own the low-level lid switch as well.
const WHAT: &str = "sleep:handle-lid-switch";
static INHIBITOR: Mutex<Option<zvariant::OwnedFd>> = Mutex::const_new(None);

pub(super) fn is_ours(item: &RawInhibitor) -> bool {
    item.1 == WHO
        && item.3 == "block"
        && item.5 == std::process::id()
        && item.0.split(':').any(|part| part == "sleep")
}

pub(crate) async fn set_keep_awake(enabled: bool) -> Result<PowerSleepState> {
    // Serialize with manual, idle and managed lid actions: enabling cannot race
    // a sleep request between its final checks and logind call.
    let _guard = SLEEP_ACTION
        .try_lock()
        .context("a lock or sleep request is already in progress")?;
    let mut inhibitor = INHIBITOR.lock().await;
    if !enabled {
        // Releasing protection must not depend on telemetry or bus health.
        *inhibitor = None;
    }
    match zbus::Connection::system().await {
        Ok(connection) => set_connected(&connection, &mut inhibitor, enabled).await,
        Err(error) if !enabled => Ok(PowerSleepState {
            error: Some(format!(
                "Keep awake disabled, but status refresh failed: {error}"
            )),
            lock_before_sleep: true,
            ..PowerSleepState::default()
        }),
        Err(error) => Err(error).context("connect to system D-Bus"),
    }
}

async fn set_connected(
    connection: &zbus::Connection,
    inhibitor: &mut Option<zvariant::OwnedFd>,
    enabled: bool,
) -> Result<PowerSleepState> {
    let mut current = PowerSleepState {
        lock_before_sleep: true,
        ..PowerSleepState::default()
    };
    if enabled {
        current = read_state(connection, false).await?;
        ensure_not_preparing(connection).await?;
        if inhibitor.is_none() || !current.keep_awake {
            // Acquire before replacing an old FD; failure leaves existing
            // protection intact. Reacquire if logind restarted underneath us.
            let fd = manager(connection)
                .await?
                .call("Inhibit", &(WHAT, WHO, WHY, "block"))
                .await
                .context("enable Keep awake through systemd-logind")?;
            *inhibitor = Some(fd);
        }
    } else {
        *inhibitor = None;
    }
    // A telemetry failure after the effect is not a failed toggle inviting a
    // second effect. The FD acquisition/release itself is authoritative.
    current = read_state(connection, false).await.unwrap_or_else(|error| {
        current.available = false;
        current.keep_awake = enabled;
        current.error = Some(format!(
            "Keep awake updated, but status refresh failed: {error:#}"
        ));
        current
    });
    if !enabled {
        current.keep_awake = false;
        // logind may not have processed the closed FD yet.
        current
            .inhibitors
            .retain(|item| item.who != WHO || item.pid != std::process::id());
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sleep::lock_tests::{SessionState, fake_logind};
    use std::sync::{Arc, atomic::Ordering};

    #[tokio::test]
    async fn inhibitor_is_idempotent_and_lives_until_released() {
        let state = Arc::new(SessionState::default());
        let (_server, client) = fake_logind(Arc::clone(&state)).await;
        let mut fd = None;
        assert!(
            set_connected(&client, &mut fd, true)
                .await
                .unwrap()
                .keep_awake
        );
        assert!(fd.is_some());
        assert!(
            set_connected(&client, &mut fd, true)
                .await
                .unwrap()
                .keep_awake
        );
        assert_eq!(state.inhibit_calls.load(Ordering::SeqCst), 1);
        assert!(
            !set_connected(&client, &mut fd, false)
                .await
                .unwrap()
                .keep_awake
        );
        assert!(fd.is_none());
        let peer = state.inhibitor_peer.lock().unwrap().take().unwrap();
        let mut byte = [0];
        assert_eq!(std::io::Read::read(&mut &peer, &mut byte).unwrap(), 0);
    }

    #[tokio::test]
    async fn lost_inhibitor_is_reacquired_and_release_does_not_need_telemetry() {
        let state = Arc::new(SessionState::default());
        let (_server, client) = fake_logind(Arc::clone(&state)).await;
        let mut fd = None;
        set_connected(&client, &mut fd, true).await.unwrap();
        state.keep_awake.store(false, Ordering::SeqCst); // logind lost ownership
        assert!(!read_state(&client, false).await.unwrap().keep_awake);
        set_connected(&client, &mut fd, true).await.unwrap();
        assert_eq!(state.inhibit_calls.load(Ordering::SeqCst), 2);
        state.telemetry_fails.store(true, Ordering::SeqCst);
        let result = set_connected(&client, &mut fd, false).await.unwrap();
        assert!(!result.keep_awake);
        assert!(!result.available);
        assert!(result.error.is_some());
        assert!(fd.is_none());
    }

    #[test]
    fn only_our_own_strong_sleep_inhibitor_is_reported() {
        let ours = (
            WHAT.into(),
            WHO.into(),
            WHY.into(),
            "block".into(),
            1000,
            std::process::id(),
        );
        assert!(is_ours(&ours));
        let mut other = ours.clone();
        other.5 += 1;
        assert!(!is_ours(&other));
        other = ours.clone();
        other.0 = "handle-lid-switch".into();
        assert!(!is_ours(&other));
        other = ours.clone();
        other.3 = "delay".into();
        assert!(!is_ours(&other));
        other = ours;
        other.1 = "Other application".into();
        assert!(!is_ours(&other));
    }

    #[tokio::test]
    async fn preparing_or_denied_inhibition_never_claims_success() {
        for preparing in [true, false] {
            let state = Arc::new(SessionState::default());
            state.preparing.store(preparing, Ordering::SeqCst);
            state.inhibit_fails.store(true, Ordering::SeqCst);
            let (_server, client) = fake_logind(state).await;
            let mut fd = None;
            assert!(set_connected(&client, &mut fd, true).await.is_err());
            assert!(fd.is_none());
        }
    }
}
