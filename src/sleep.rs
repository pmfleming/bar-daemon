use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::{sync::Mutex, time::sleep};

use crate::model::{PowerSleepState, SleepInhibitor};

pub(crate) mod diagnostics;
mod keep_awake;
mod monitoring;
mod resume;
mod wayland_lock;

pub(crate) use keep_awake::set_keep_awake;
pub(crate) use monitoring::monitor;

fn is_hyprland_session() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        || std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktop| {
            desktop
                .split(':')
                .any(|part| part.eq_ignore_ascii_case("Hyprland"))
        })
}

pub(crate) async fn inspect_lock() -> Result<bool> {
    wayland_lock::LockObserver::connect().await?.locked().await
}

const BUS: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
const SESSION_PATH: &str = "/org/freedesktop/login1/session/auto";
const SESSION_INTERFACE: &str = "org.freedesktop.login1.Session";
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const DEPENDENCY_TIMEOUT: Duration = Duration::from_secs(3);
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(20);

/// Bound connection setup as well as every method on action/policy connections.
/// Dispatch timeouts are ambiguous outcomes: never automatically replay them.
pub(crate) async fn system_bus() -> Result<zbus::Connection> {
    bounded("connect to system D-Bus", DEPENDENCY_TIMEOUT, async {
        Ok(zbus::connection::Builder::system()?
            .method_timeout(DEPENDENCY_TIMEOUT)
            .build()
            .await?)
    })
    .await
}

pub(crate) async fn bounded<T>(
    label: &str,
    deadline: Duration,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(deadline, future)
        .await
        .with_context(|| format!("{label} timed out"))?
}
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);
static SLEEP_ACTION: Mutex<()> = Mutex::const_new(());

#[cfg(test)]
#[path = "sleep_tests.rs"]
mod lock_tests;

type RawInhibitor = (String, String, String, String, u32, u32);

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
    // Do not overwrite an in-progress sleep with a stale caller-supplied false.
    let preparing_for_sleep = proxy
        .get_property("PreparingForSleep")
        .await
        .unwrap_or(preparing_for_sleep);
    Ok(PowerSleepState {
        available: true,
        can_suspend,
        can_hibernate,
        preparing_for_sleep,
        resume_generation: 0, // StateStore preserves the resident generation.
        lock_before_sleep: true,
        keep_awake: inhibitors.iter().any(keep_awake::is_ours),
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
        diagnostics: diagnostics::system(),
    })
}

pub(crate) async fn manager(connection: &zbus::Connection) -> Result<zbus::Proxy<'_>> {
    zbus::proxy::Builder::new(connection)
        .destination(BUS)?
        .path(MANAGER_PATH)?
        .interface(MANAGER_INTERFACE)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .context("connect to systemd-logind")
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Lock,
    Suspend,
    Hibernate,
    SuspendThenHibernate,
}

impl Action {
    fn parse(action: &str) -> Result<Self> {
        match action {
            "lock" => Ok(Self::Lock),
            "suspend" => Ok(Self::Suspend),
            "hibernate" => Ok(Self::Hibernate),
            "suspend-then-hibernate" => Ok(Self::SuspendThenHibernate),
            _ => bail!("unsupported power and sleep action: {action}"),
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::Lock => "Lock",
            Self::Suspend => "Suspend",
            Self::Hibernate => "Hibernate",
            Self::SuspendThenHibernate => "SuspendThenHibernate",
        }
    }
}

pub(crate) async fn perform(action: &str) -> Result<PowerSleepState> {
    perform_with_setup(action, || std::future::ready(Ok(()))).await
}

/// Setup (e.g. a privileged hibernate delay write) happens only after locking,
/// under the same action guard as manual sleep. Never mutate it before preflight.
pub(crate) async fn perform_with_setup<F, Fut>(action: &str, setup: F) -> Result<PowerSleepState>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    Action::parse(action)?;
    let _guard = SLEEP_ACTION
        .try_lock()
        .context("a lock or sleep request is already in progress")?;
    let connection = system_bus().await?;
    perform_connected_with_setup(
        &connection,
        action,
        LOCK_TIMEOUT,
        is_hyprland_session(),
        setup,
    )
    .await
}

#[cfg(test)]
async fn perform_connected(
    connection: &zbus::Connection,
    action: &str,
    lock_timeout: Duration,
) -> Result<PowerSleepState> {
    perform_connected_with_setup(connection, action, lock_timeout, false, || {
        std::future::ready(Ok(()))
    })
    .await
}

async fn perform_connected_with_setup<F, Fut>(
    connection: &zbus::Connection,
    action: &str,
    lock_timeout: Duration,
    use_wayland: bool,
    setup: F,
) -> Result<PowerSleepState>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let action = Action::parse(action)?;
    let (mut current, _observer) = bounded(
        "sleep preflight (no sleep request sent)",
        PREFLIGHT_TIMEOUT,
        async {
            let current = read_state(connection, false).await?;
            if action != Action::Lock {
                ensure_not_preparing(connection).await?;
                ensure_sleep_allowed(&current)?;
            }
            if action == Action::Suspend && !capability_available(&current.can_suspend) {
                bail!("suspend is unavailable: {}", current.can_suspend);
            }
            if action == Action::Hibernate && !capability_available(&current.can_hibernate) {
                bail!(
                    "hibernate is unavailable: {}. {}",
                    current.can_hibernate,
                    current.diagnostics.hibernate_issues.join(" ")
                );
            }
            if action == Action::SuspendThenHibernate {
                check_combined_capability(connection).await?;
            }
            // Resolve 'auto' once and keep the concrete session object through the
            // complete operation. A VT/session switch must not lock one session and
            // then read a different session's hint.
            let mut observer = if use_wayland {
                Some(wayland_lock::LockObserver::connect().await?)
            } else {
                None
            };
            let session = lock_session(
                connection,
                lock_timeout,
                action != Action::Lock,
                &mut observer,
            )
            .await?;
            if action != Action::Lock {
                setup().await?;
                ensure_sleep_allowed(&read_state(connection, false).await?)?;
                ensure_active(&session).await?;
                if !confirmed_locked(&session, &mut observer).await? {
                    bail!("session unlocked before the sleep request; refusing to sleep");
                }
                ensure_not_preparing(connection).await?;
            }
            Ok((current, observer))
        },
    )
    .await?;
    if action != Action::Lock {
        manager(connection).await?.call_method(action.method(), &(false,)).await
            .with_context(|| format!("could not confirm {} through systemd-logind; a lost reply may mean the action was already accepted. Check the session before retrying", action.method()))?;
    }
    // A successful effect is not undone by an unrelated telemetry failure.
    // Returning Err here used to invite a second, potentially destructive Retry.
    match bounded(
        "post-action status",
        DEPENDENCY_TIMEOUT,
        read_state(connection, action != Action::Lock),
    )
    .await
    {
        Ok(state) => Ok(state),
        Err(error) => {
            current.available = false;
            current.preparing_for_sleep = action != Action::Lock;
            current.error = Some(format!(
                "{} accepted, but status refresh failed: {error:#}",
                action.method()
            ));
            Ok(current)
        }
    }
}

fn ensure_sleep_allowed(state: &PowerSleepState) -> Result<()> {
    if state.keep_awake {
        bail!("Keep awake is enabled; turn it off before sleeping");
    }
    Ok(())
}

async fn ensure_not_preparing(connection: &zbus::Connection) -> Result<()> {
    // Unlike display telemetry, action preflight must not turn a missing or
    // unreadable property into a false value.
    let preparing: bool = manager(connection)
        .await?
        .get_property("PreparingForSleep")
        .await
        .context("cannot confirm whether logind is preparing for sleep; refusing to sleep")?;
    if preparing {
        bail!("the system is already preparing for sleep");
    }
    Ok(())
}

async fn ensure_active(session: &zbus::Proxy<'_>) -> Result<()> {
    if !session
        .get_property::<bool>("Active")
        .await
        .context("verify active session")?
    {
        bail!("the requested session is no longer active; refusing to sleep");
    }
    Ok(())
}

async fn session_proxy<'a>(
    connection: &'a zbus::Connection,
    path: zvariant::OwnedObjectPath,
) -> Result<zbus::Proxy<'a>> {
    zbus::proxy::Builder::new(connection)
        .destination(BUS)?
        .path(path)?
        .interface(SESSION_INTERFACE)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .context("connect to the logind session")
}

pub(crate) async fn current_session(connection: &zbus::Connection) -> Result<zbus::Proxy<'_>> {
    let auto = session_proxy(
        connection,
        zvariant::OwnedObjectPath::try_from(SESSION_PATH)?,
    )
    .await?;
    let id: String = auto
        .get_property("Id")
        .await
        .context("resolve current session identity")?;
    let path: zvariant::OwnedObjectPath = manager(connection)
        .await?
        .call("GetSession", &(id,))
        .await
        .context("resolve concrete logind session")?;
    session_proxy(connection, path).await
}

async fn confirmed_locked(
    session: &zbus::Proxy<'_>,
    observer: &mut Option<wayland_lock::LockObserver>,
) -> Result<bool> {
    match observer {
        Some(observer) => observer.locked().await,
        None => session
            .get_property("LockedHint")
            .await
            .context("read session lock confirmation"),
    }
}

async fn lock_session<'a>(
    connection: &'a zbus::Connection,
    deadline: Duration,
    require_active: bool,
    observer: &mut Option<wayland_lock::LockObserver>,
) -> Result<zbus::Proxy<'a>> {
    tokio::time::timeout(deadline, async {
        let session = current_session(connection).await?;
        if require_active {
            ensure_active(&session).await?;
        }
        if !confirmed_locked(&session, observer).await? {
            session
                .call_method("Lock", &())
                .await
                .context("request session lock")?;
        }
        loop {
            if confirmed_locked(&session, observer).await? {
                return Ok(session);
            }
            sleep(LOCK_POLL_INTERVAL).await;
        }
    })
    .await
    .context("session lock was not confirmed before the deadline; refusing to sleep")?
}

pub(crate) async fn check_suspend_then_hibernate() -> Result<()> {
    let connection = system_bus().await?;
    check_combined_capability(&connection).await
}

async fn check_combined_capability(connection: &zbus::Connection) -> Result<()> {
    let capability: String = manager(connection)
        .await?
        .call("CanSuspendThenHibernate", &())
        .await
        .context("read suspend-then-hibernate capability")?;
    if !capability_available(&capability) {
        bail!(
            "suspend-then-hibernate is unavailable: {capability}. {}",
            diagnostics::system().hibernate_issues.join(" ")
        );
    }
    Ok(())
}

fn capability_available(value: &str) -> bool {
    matches!(value, "yes" | "challenge")
}
