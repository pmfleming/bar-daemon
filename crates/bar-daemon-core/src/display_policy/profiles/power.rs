//! Read the power owner's projections; never duplicate its hardware or lid policy.
use super::Context;
use crate::{api, component::Component, protocol::stream, state::StateStore};
use anyhow::{Result, bail, ensure};
use futures::StreamExt;
use serde_json::Value;
use std::time::Duration;

async fn publish(store: &StateStore, value: Context) {
    let mut current = store.display_context.write().await;
    if *current != value {
        *current = value;
        store.display_changed.notify_one();
    }
}
fn update(context: &mut Context, stream: &str, data: &Value) {
    match stream {
        stream::BATTERY => {
            context.plugged = if data["available"] == true && data["power_available"] == true {
                data["plugged"].as_bool()
            } else {
                None
            }
        }
        stream::SLEEP_POLICY => {
            context.lid_closed = if data["lid"]["available"] == true {
                data["lid"]["closed"].as_bool()
            } else {
                None
            }
        }
        _ => {}
    }
}
pub(crate) async fn monitor(store: StateStore) {
    if !super::super::enabled() {
        return;
    }
    loop {
        if let Err(error) = connected(&store).await {
            tracing::debug!(%error, "display power context disconnected");
        }
        publish(&store, Context::default()).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
async fn connected(store: &StateStore) -> Result<()> {
    // Connection lifetime owns the subscription; dropping it retires the lease.
    let connection = zbus::connection::Builder::session()?
        .method_timeout(crate::bus::TIMEOUT)
        .build()
        .await?;
    let proxy = zbus::Proxy::new(
        &connection,
        Component::Power.bus_name(),
        api::OBJECT_PATH,
        api::INTERFACE,
    )
    .await?;
    let mut events = proxy.receive_signal("Event").await?;
    let mut owners = proxy.receive_owner_changed().await?;
    let reply: String = proxy
        .call("Subscribe", &(vec![stream::BATTERY, stream::SLEEP_POLICY],))
        .await?;
    ensure!(
        serde_json::from_str::<Value>(&reply)?["ok"] == true,
        "power owner rejected display context subscription"
    );
    let mut context = Context::default();
    // Initial subscribed events carry ordered snapshots. Never fetch a separate
    // snapshot that could race and overwrite those events with older values.
    loop {
        tokio::select! {
            _ = owners.next() => bail!("power owner changed"),
            event = events.next() => {
                let message = event.ok_or_else(|| anyhow::anyhow!("power event stream ended"))?;
                let (stream, json): (String, String) = message.body().deserialize()?;
                let value: Value = serde_json::from_str(&json)?;
                ensure!(["changed", "subscribed"].contains(&value["event"].as_str().unwrap_or("")), "power event gap requires resubscription");
                update(&mut context, &stream, &value["data"]);
                publish(store, context.clone()).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_power_never_becomes_a_false_battery_or_open_lid_match() {
        let mut context = Context::default();
        update(
            &mut context,
            stream::BATTERY,
            &serde_json::json!({"available":true,"power_available":true,"plugged":false}),
        );
        assert_eq!(context.plugged, Some(false));
        update(
            &mut context,
            stream::BATTERY,
            &serde_json::json!({"available":false,"plugged":false}),
        );
        update(
            &mut context,
            stream::SLEEP_POLICY,
            &serde_json::json!({"lid":{"available":false,"closed":false}}),
        );
        assert_eq!(context, Context::default());
    }
}
