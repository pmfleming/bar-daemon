use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use tokio::time::{interval, sleep};

use crate::{
    model::{PowerSleepState, SleepInhibitor},
    state::StateStore,
};

const BUS: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
const SESSION_PATH: &str = "/org/freedesktop/login1/session/auto";
const SESSION_INTERFACE: &str = "org.freedesktop.login1.Session";
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[cfg(test)]
#[path = "sleep_tests.rs"]
mod lock_tests;

type RawInhibitor = (String, String, String, String, u32, u32);

pub(crate) async fn monitor(store: StateStore) {
    loop {
        match zbus::Connection::system().await {
            Ok(connection) => {
                if let Err(error) = monitor_connection(&connection, &store).await {
                    store
                        .update_power_sleep(PowerSleepState {
                            error: Some(error.to_string()),
                            lock_before_sleep: true,
                            ..PowerSleepState::default()
                        })
                        .await;
                }
            }
            Err(error) => {
                store
                    .update_power_sleep(PowerSleepState {
                        error: Some(error.to_string()),
                        lock_before_sleep: true,
                        ..PowerSleepState::default()
                    })
                    .await;
            }
        }
        sleep(Duration::from_secs(3)).await;
    }
}

async fn monitor_connection(connection: &zbus::Connection, store: &StateStore) -> Result<()> {
    let proxy = manager(connection).await?;
    let mut prepare = proxy.receive_signal("PrepareForSleep").await?;
    let mut fallback = interval(Duration::from_secs(60));
    fallback.tick().await;
    refresh(connection, store, false).await;
    loop {
        tokio::select! {
            signal = prepare.next() => {
                let Some(signal) = signal else { bail!("logind sleep signal stream ended"); };
                let (preparing,): (bool,) = signal
                    .body()
                    .deserialize()
                    .context("decode logind PrepareForSleep signal")?;
                refresh(connection, store, preparing).await;
            }
            _ = fallback.tick() => {
                let preparing = store.snapshot().await.power_sleep.preparing_for_sleep;
                refresh(connection, store, preparing).await;
            }
        }
    }
}

async fn refresh(connection: &zbus::Connection, store: &StateStore, preparing: bool) {
    match read_state(connection, preparing).await {
        Ok(state) => store.update_power_sleep(state).await,
        Err(error) => {
            store
                .update_power_sleep(PowerSleepState {
                    error: Some(error.to_string()),
                    preparing_for_sleep: preparing,
                    lock_before_sleep: true,
                    ..PowerSleepState::default()
                })
                .await;
        }
    }
}

async fn read_state(
    connection: &zbus::Connection,
    preparing_for_sleep: bool,
) -> Result<PowerSleepState> {
    let proxy = manager(connection).await?;
    let can_suspend: String = proxy
        .call("CanSuspend", &())
        .await
        .context("read suspend capability")?;
    let can_hibernate: String = proxy
        .call("CanHibernate", &())
        .await
        .context("read hibernate capability")?;
    let inhibitors: Vec<RawInhibitor> = proxy
        .call("ListInhibitors", &())
        .await
        .context("list logind inhibitors")?;
    Ok(PowerSleepState {
        available: true,
        can_suspend,
        can_hibernate,
        preparing_for_sleep,
        lock_before_sleep: true,
        inhibitors: inhibitors
            .into_iter()
            .map(|(what, who, why, mode, uid, pid)| SleepInhibitor {
                what,
                who,
                why,
                mode,
                uid,
                pid,
            })
            .collect(),
        error: None,
    })
}

async fn manager(connection: &zbus::Connection) -> Result<zbus::Proxy<'_>> {
    zbus::Proxy::new(connection, BUS, MANAGER_PATH, MANAGER_INTERFACE)
        .await
        .context("connect to systemd-logind")
}

pub(crate) async fn perform(action: &str) -> Result<PowerSleepState> {
    if !matches!(action, "lock" | "suspend" | "hibernate") {
        bail!("unsupported power and sleep action: {action}");
    }
    let connection = zbus::Connection::system()
        .await
        .context("connect to system D-Bus")?;
    perform_connected(&connection, action, LOCK_TIMEOUT).await
}

async fn perform_connected(
    connection: &zbus::Connection,
    action: &str,
    lock_timeout: Duration,
) -> Result<PowerSleepState> {
    let current = read_state(connection, false).await?;
    if action == "suspend" && !capability_available(&current.can_suspend) {
        bail!("suspend is unavailable: {}", current.can_suspend);
    }
    if action == "hibernate" && !capability_available(&current.can_hibernate) {
        bail!("hibernate is unavailable: {}", current.can_hibernate);
    }
    lock_session(connection, lock_timeout).await?;
    if action != "lock" {
        manager(connection)
            .await?
            .call_method(
                if action == "suspend" {
                    "Suspend"
                } else {
                    "Hibernate"
                },
                &(false,),
            )
            .await
            .with_context(|| format!("request {action} through systemd-logind"))?;
    }
    read_state(connection, false).await
}

async fn lock_session(connection: &zbus::Connection, deadline: Duration) -> Result<()> {
    tokio::time::timeout(deadline, async {
        // Lock only emits a request to the session's locker. LockedHint must be
        // set by that integration *after* the compositor confirms the lock.
        // Read directly: a cached hint is not a lock-completion acknowledgement.
        let session: zbus::Proxy<'_> = zbus::proxy::Builder::new(connection)
            .destination(BUS)?
            .path(SESSION_PATH)?
            .interface(SESSION_INTERFACE)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .context("connect to the active logind session")?;
        session
            .call_method("Lock", &())
            .await
            .context("request session lock")?;
        loop {
            if session
                .get_property::<bool>("LockedHint")
                .await
                .context("read session lock confirmation")?
            {
                return Ok(());
            }
            sleep(LOCK_POLL_INTERVAL).await;
        }
    })
    .await
    .context("session lock was not confirmed before the deadline; refusing to sleep")?
}

fn capability_available(value: &str) -> bool {
    matches!(value, "yes" | "challenge")
}
