//! Shared client connections, not shared method execution. A lost connection is
//! replaced on the next acquisition; a timed-out/effectful call is never replayed.
use std::{future::Future, time::Duration};

use anyhow::{Context, Result};
use futures::StreamExt;
use tokio::{sync::Mutex, task::JoinHandle};
use zbus::{Connection, MessageStream, connection::Builder};

pub(crate) const TIMEOUT: Duration = Duration::from_secs(3);
static SYSTEM: ClientBus = ClientBus::new();
static SESSION: ClientBus = ClientBus::new();

struct Cached {
    connection: Connection,
    // zbus 5.13 has no is_closed(). Drain its transport stream continuously to
    // observe disconnects, without polling the bus or adding a round trip.
    closed: JoinHandle<()>,
}
impl Cached {
    fn new(connection: Connection) -> Self {
        let mut messages = MessageStream::from(&connection);
        let closed = tokio::spawn(async move { while let Some(Ok(_)) = messages.next().await {} });
        Self { connection, closed }
    }
}
impl Drop for Cached {
    fn drop(&mut self) {
        self.closed.abort();
    }
}

struct ClientBus(Mutex<Option<Cached>>);
impl ClientBus {
    const fn new() -> Self {
        Self(Mutex::const_new(None))
    }

    async fn get<F, Fut>(&self, connect: F) -> Result<Connection>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Connection>>,
    {
        tokio::time::timeout(TIMEOUT, async {
            let mut cached = self.0.lock().await;
            if let Some(current) = &*cached {
                if !current.closed.is_finished() {
                    return Ok(current.connection.clone());
                }
            }
            // Single-flight reconnect. Cancellation/failure leaves no poisoned
            // initialization state, and callers never hold this mutex for RPCs.
            *cached = None;
            let connection = connect().await?;
            *cached = Some(Cached::new(connection.clone()));
            Ok(connection)
        })
        .await
        .context("D-Bus connection acquisition timed out")?
    }
}

pub(crate) async fn system() -> Result<Connection> {
    SYSTEM
        .get(system_owned)
        .await
        .context("connect to system D-Bus")
}

pub(crate) async fn session() -> Result<Connection> {
    SESSION
        .get(|| async { Ok(Builder::session()?.method_timeout(TIMEOUT).build().await?) })
        .await
        .context("connect to session D-Bus")
}

/// For connection-scoped resources (e.g. PPD hold cookies), dropping the owner
/// must still disconnect. Do not put these lifetime-bearing owners in the cache.
pub(crate) async fn system_owned() -> Result<Connection> {
    tokio::time::timeout(TIMEOUT, async {
        Ok(Builder::system()?.method_timeout(TIMEOUT).build().await?)
    })
    .await
    .context("connect to system D-Bus timed out")?
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Service;
    #[zbus::interface(name = "org.example.Test")]
    impl Service {
        async fn stall(&self) {
            std::future::pending::<()>().await;
        }
        fn ping(&self) -> u32 {
            7
        }
    }
    async fn peer() -> (Connection, Connection) {
        let (server, client) = tokio::net::UnixStream::pair().unwrap();
        tokio::try_join!(
            Builder::unix_stream(server)
                .server(zbus::Guid::generate())
                .unwrap()
                .p2p()
                .serve_at("/org/example/Test", Service)
                .unwrap()
                .build(),
            Builder::unix_stream(client)
                .p2p()
                .method_timeout(TIMEOUT)
                .build(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn method_timeout_does_not_disconnect_or_replay_and_other_calls_can_progress() {
        let cache = ClientBus::new();
        let (_server, client) = peer().await;
        let connection = cache.get(|| async { Ok(client) }).await.unwrap();
        let proxy = zbus::Proxy::new(
            &connection,
            "org.example.Test",
            "/org/example/Test",
            "org.example.Test",
        )
        .await
        .unwrap();
        let (stall, ping) = tokio::join!(
            proxy.call::<_, _, ()>("Stall", &()),
            proxy.call::<_, _, u32>("Ping", &())
        );
        assert!(stall.is_err());
        assert_eq!(ping.unwrap(), 7);
        let _reused = cache
            .get(|| async { panic!("an RPC timeout must not recreate the connection") })
            .await
            .unwrap();
    }
}
