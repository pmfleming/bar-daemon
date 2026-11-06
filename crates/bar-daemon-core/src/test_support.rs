//! Private peer fixtures: no session/system bus names or host services involved.
use zbus::{Connection, connection::Builder};

pub(crate) async fn dbus_peer(
    configure: impl FnOnce(Builder<'static>) -> zbus::Result<Builder<'static>>,
) -> zbus::Result<(Connection, Connection)> {
    let (server, client) = tokio::net::UnixStream::pair()?;
    let server = configure(
        Builder::unix_stream(server)
            .server(zbus::Guid::generate())?
            .p2p(),
    )?
    .build();
    let client = Builder::unix_stream(client).p2p().build();
    // Both handshakes must progress together. The caller retains the server
    // connection for exactly as long as its exported interfaces are needed.
    tokio::try_join!(server, client)
}
