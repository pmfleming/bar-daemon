//! Kernel/socket lifetime and D-Bus name lifetime are distinct authorities.
use shelllist_daemon_tokio::{OwnerLossMonitor, SocketSession};

#[derive(Clone)]
pub(super) enum Owner {
    Bus {
        name: String,
        namespace: String,
        monitor: OwnerLossMonitor,
    },
    Socket(SocketSession),
}
impl Owner {
    pub(super) fn bus(
        connection: &zbus::Connection,
        name: String,
        monitor: OwnerLossMonitor,
    ) -> Self {
        Self::Bus {
            name,
            namespace: format!(
                "{}-{}",
                connection.server_guid(),
                connection
                    .unique_name()
                    .map(|n| n.as_str())
                    .unwrap_or("peer")
            ),
            monitor,
        }
    }
    pub(super) fn name(&self) -> &str {
        match self {
            Self::Bus { name, .. } => name,
            Self::Socket(session) => session.owner(),
        }
    }
    pub(super) fn namespace(&self) -> &str {
        match self {
            Self::Bus { namespace, .. } => namespace,
            Self::Socket(session) => session.owner(),
        }
    }
    pub(super) async fn exists(&self) -> bool {
        match self {
            Self::Bus { monitor, name, .. } => matches!(monitor.has_owner(name).await, Ok(true)),
            Self::Socket(session) => !session.is_closed(),
        }
    }
    pub(super) async fn closed(&self) {
        match self {
            Self::Bus { monitor, name, .. } => {
                let _ = monitor.wait(name).await;
            }
            Self::Socket(session) => session.closed().await,
        }
    }
}
