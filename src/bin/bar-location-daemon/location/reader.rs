//! Read-only access to the last durably accepted fix, restricted to active local users.
use super::storage::Storage;
use std::{collections::HashMap, time::Duration};
use zbus::{Connection, message::Header};
use zvariant::Value;

pub(super) struct Reader(pub Storage);

#[zbus::interface(name = "org.laufan.LocationTimezone")]
impl Reader {
    async fn get_accepted(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<String> {
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::AccessDenied("missing caller identity".into()))?;
        // Never request authentication UI. The policy permits only an active
        // local session; inactive/remote callers cannot read coordinates.
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
            proxy
                .call::<_, _, (bool, bool, HashMap<String, String>)>(
                    "CheckAuthorization",
                    &(
                        subject,
                        "org.laufan.location-timezone.read",
                        HashMap::<&str, &str>::new(),
                        0_u32,
                        "",
                    ),
                )
                .await
        })
        .await;
        if !matches!(authorized, Ok(Ok((true, _, _)))) {
            return Err(zbus::fdo::Error::AccessDenied(
                "accepted location requires an active local session".into(),
            ));
        }
        // Read the private durable state, never in-flight or rejected evidence.
        let state = self
            .0
            .load()
            .map_err(|_| zbus::fdo::Error::Failed("accepted location state unavailable".into()))?;
        serde_json::to_string(&state.accepted)
            .map_err(|_| zbus::fdo::Error::Failed("accepted location encoding failed".into()))
    }
}
