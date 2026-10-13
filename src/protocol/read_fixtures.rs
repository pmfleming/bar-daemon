//! Offline fixture: typed example state -> real StateStore commits -> live wire constructors.
use anyhow::{Result, ensure};
use serde_json::{Map, Value};
use tokio::sync::broadcast::error::TryRecvError;

use super::{
    STREAMS,
    read_wire::{self, Event},
};
use crate::{model::BarSnapshot, state::StateStore};

const SUBSCRIPTION: &str = "subscription-example";

pub(super) async fn fixture() -> Result<Value> {
    // The existing examples supply sample values only. Never copy their JSON
    // directly onto the wire: domain types and production serializers own it.
    let mut sample: BarSnapshot =
        serde_json::from_value(super::contract_fixture()?["snapshot"].take())?;
    sample.workarea.available = true;
    sample.workarea.monitors.insert(
        "eDP-1".into(),
        shelllist_hyprland::work_area::Insets {
            left: 0.0,
            top: 28.0,
            right: 0.0,
            bottom: 0.0,
        },
    );
    let state = StateStore::default();
    let (initial, mut events) = state.snapshot_and_subscribe().await;
    state.update_activity(sample.activity).await;
    state.update_workspaces(sample.workspaces).await;
    state.update_work_area(sample.workarea).await;
    state.update_compositor(sample.compositor).await;
    state.update_media(sample.media).await;
    state.update_audio(sample.audio).await;
    state.update_brightness(sample.brightness).await;
    state.update_battery(sample.battery).await;
    state.update_power_profile(sample.power_profile).await;
    state.update_power_sleep(sample.power_sleep).await;
    // Independently-owned observations must go through their own commit paths.
    state.update_lid(sample.sleep_policy.lid.clone()).await;
    state
        .update_critical_battery(sample.sleep_policy.critical_battery.clone())
        .await;
    state.update_sleep_policy(sample.sleep_policy).await;
    state.update_display_policy(sample.display_policy).await;
    state.update_osd_hardware(sample.osd_hardware).await;
    state.update_notifications(sample.notifications).await;
    state
        .update_notification_active(sample.notification_active)
        .await;
    state.update_updates(sample.updates).await;
    state.update_timezone(sample.timezone).await;

    let mut changed = Map::new();
    loop {
        match events.try_recv() {
            Ok(update) => {
                let event = Event::changed(SUBSCRIPTION, update);
                changed.insert(event.stream().into(), event.into_value());
            }
            Err(TryRecvError::Empty) => break,
            Err(error) => return Err(error.into()),
        }
    }
    ensure!(
        changed.len() == STREAMS.len()
            && STREAMS.iter().all(|stream| changed.contains_key(*stream)),
        "Every registered stream must have a real changed event in the read fixture"
    );
    let subscribed: Map<String, Value> = STREAMS
        .iter()
        .map(|stream| {
            (
                (*stream).into(),
                Event::subscribed(stream, SUBSCRIPTION, &initial).into_value(),
            )
        })
        .collect();
    let lagged: Map<String, Value> = STREAMS
        .iter()
        .map(|stream| {
            (
                (*stream).into(),
                Event::lagged(stream, SUBSCRIPTION, 4).into_value(),
            )
        })
        .collect();
    let mut fixture = Map::new();
    fixture.insert(
        "snapshot".into(),
        read_wire::snapshot(&state.snapshot().await),
    );
    fixture.insert("subscribed".into(), Value::Object(subscribed));
    fixture.insert("changed".into(), Value::Object(changed));
    fixture.insert("lagged".into(), Value::Object(lagged));
    state
        .monitor_failed("audio", &[super::stream::AUDIO], "panicked")
        .await;
    let failed = events.try_recv()?;
    ensure!(
        failed.stream == super::stream::AUDIO,
        "monitor failure must stay domain-local"
    );
    fixture.insert(
        "monitor_failed".into(),
        Event::changed(SUBSCRIPTION, failed).into_value(),
    );
    Ok(Value::Object(fixture))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn committed_changes_and_resubscription_project_the_same_snapshot_for_every_stream()
    -> Result<()> {
        let fixture = fixture().await?;
        let snapshot: BarSnapshot =
            serde_json::from_value(fixture["snapshot"]["data"]["snapshot"].clone())?;
        for stream in STREAMS {
            let changed = &fixture["changed"][stream];
            let resubscribed = Event::subscribed(stream, SUBSCRIPTION, &snapshot).into_value();
            assert_eq!(
                changed["data"], resubscribed["data"],
                "{stream}: commit and initial projection drifted"
            );
            assert_eq!(changed["event"], "changed");
            assert_eq!(changed["stream"], *stream);
            assert_eq!(changed["subscription_id"], SUBSCRIPTION);
            assert_eq!(fixture["subscribed"][stream]["data"]["available"], false);
            assert_eq!(fixture["lagged"][stream]["event"], "gap");
            assert_eq!(fixture["lagged"][stream]["data"]["dropped_events"], "4");
        }
        assert_eq!(
            snapshot.workarea.revision, 1,
            "Store-owned revisions must not be invented by fixtures"
        );
        assert_eq!(snapshot.compositor.revision, 1);
        Ok(())
    }
}
