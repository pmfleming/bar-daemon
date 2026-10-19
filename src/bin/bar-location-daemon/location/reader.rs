//! Private reads and narrow controls restricted to the actual active local caller.
use super::{
    Control,
    control::{self, Command, Request},
    storage::Storage,
};
use std::{collections::HashMap, time::Duration};
use tokio::sync::{mpsc, oneshot, watch};
use zbus::{Connection, message::Header};
use zvariant::Value;

pub(super) struct Reader {
    pub storage: Storage,
    pub commands: mpsc::Sender<Command>,
    pub changes: watch::Sender<Control>,
}
async fn authorize(
    header: &Header<'_>,
    connection: &Connection,
    action: &str,
) -> zbus::fdo::Result<u32> {
    let sender = header
        .sender()
        .ok_or_else(|| zbus::fdo::Error::AccessDenied("Missing caller identity".into()))?;
    let authorized = tokio::time::timeout(Duration::from_secs(3), async {
        let proxy = zbus::Proxy::new(
            connection,
            "org.freedesktop.PolicyKit1",
            "/org/freedesktop/PolicyKit1/Authority",
            "org.freedesktop.PolicyKit1.Authority",
        )
        .await?;
        let subject = (
            "system-bus-name",
            HashMap::from([("name", Value::from(sender.as_str()))]),
        );
        let (allowed, _, _): (bool, bool, HashMap<String, String>) = proxy
            .call(
                "CheckAuthorization",
                &(subject, action, HashMap::<&str, &str>::new(), 0_u32, ""),
            )
            .await?;
        if !allowed {
            return Err(zbus::Error::Failure("Not authorized".into()));
        }
        zbus::fdo::DBusProxy::new(connection)
            .await?
            .get_connection_unix_user(sender.clone().into())
            .await
            .map_err(Into::into)
    })
    .await;
    match authorized {
        Ok(Ok(uid)) => Ok(uid),
        _ => Err(zbus::fdo::Error::AccessDenied(
            "Location access requires an active local session".into(),
        )),
    }
}
#[zbus::interface(name = "org.laufan.LocationTimezone")]
impl Reader {
    async fn get_accepted(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<String> {
        authorize(&header, connection, "org.laufan.location-timezone.read").await?;
        let state = self
            .storage
            .load()
            .map_err(|_| zbus::fdo::Error::Failed("Location state unavailable".into()))?;
        serde_json::to_string(&state.accepted)
            .map_err(|_| zbus::fdo::Error::Failed("Location encoding failed".into()))
    }
    async fn get_control(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<String> {
        authorize(&header, connection, "org.laufan.location-timezone.read").await?;
        let state = self
            .storage
            .load()
            .map_err(|_| zbus::fdo::Error::Failed("Location state unavailable".into()))?;
        Ok(control::status(&state).to_string())
    }
    async fn set_location_policy(
        &self,
        json: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<String> {
        let uid = authorize(&header, connection, "org.laufan.location-timezone.control").await?;
        if json.len() > 4096 {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Location request too large".into(),
            ));
        }
        let request: Request = serde_json::from_str(json)
            .map_err(|_| zbus::fdo::Error::InvalidArgs("Invalid location request".into()))?;
        let (reply, received) = oneshot::channel();
        let permit = self
            .commands
            .try_reserve()
            .map_err(|_| zbus::fdo::Error::Failed("Location control busy or unavailable".into()))?;
        // Publish queue membership and its epoch under the same watch lock:
        // the worker cannot start a command before its cancellation fence exists.
        self.changes.send_modify(|state| {
            state.epoch = state.epoch.wrapping_add(1);
            permit.send(Command {
                uid,
                request,
                reply,
            });
        });
        match tokio::time::timeout(Duration::from_secs(25), received).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(message))) => Err(zbus::fdo::Error::Failed(message)),
            _ => Err(zbus::fdo::Error::Failed(
                "Location outcome unknown; read status without replaying".into(),
            )),
        }
    }
}
