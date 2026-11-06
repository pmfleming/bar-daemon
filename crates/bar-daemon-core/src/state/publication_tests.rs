use super::*;
use crate::protocol::{read_wire, stream};
use serde_json::json;

#[tokio::test]
async fn snapshots_events_health_and_effect_observations_share_only_atomic_publications() {
    let store = StateStore::default();
    let (initial, mut events) = store.snapshot_and_subscribe_domains(&[stream::AUDIO]).await;
    let before = read_wire::snapshot(&initial);
    let available = AudioState {
        available: true,
        volume_percent: 42,
        ..Default::default()
    };
    store.update_audio(available.clone()).await;
    let captured = store.snapshot().await;
    store.update_audio(available).await; // unchanged does not advance
    assert_eq!(store.snapshot().await.publications, captured.publications);
    store
        .update_audio(AudioState {
            available: true,
            volume_percent: 70,
            ..Default::default()
        })
        .await;
    let latest = events.try_recv().unwrap(); // coalesced replacement, not a delta hole
    assert_eq!(latest.data["volume_percent"], 70);
    assert_eq!(latest.data["_publication"]["revision"], 3);
    assert_eq!(
        before["data"]["snapshot"]["audio"]["_publication"]["revision"],
        1
    );
    assert_eq!(
        read_wire::snapshot(&captured)["data"]["snapshot"]["audio"]["volume_percent"],
        42
    );
    assert_eq!(
        initial.publications[stream::BATTERY],
        captured.publications[stream::BATTERY]
    );
    let receipt = crate::api::success(json!({"audio": {"volume_percent":42}, "acknowledged":true}));
    let reconciled = store.reconcile_reply(receipt.clone()).await;
    assert_eq!(reconciled["data"]["audio"], receipt["data"]["audio"]);
    assert_eq!(reconciled["data"]["snapshot"]["audio"], latest.data);
    store
        .monitor_failed("audio", &[stream::AUDIO], "panicked")
        .await;
    let unavailable = events.try_recv().unwrap();
    assert_eq!(unavailable.data["available"], false);
    assert_eq!(unavailable.data["_publication"]["revision"], 4);
    assert_eq!(
        read_wire::snapshot(&store.snapshot().await)["data"]["snapshot"]["audio"],
        unavailable.data
    );
    assert_eq!(
        read_wire::snapshot(&captured)["data"]["snapshot"]["audio"]["available"],
        true
    );
}

#[tokio::test]
async fn rollover_lineage_survives_coalescing_and_new_stores_have_new_epochs() {
    let store = StateStore::default();
    let old_epoch = store.snapshot().await.publications[stream::AUDIO]
        .epoch
        .clone();
    store
        .snapshot
        .write()
        .await
        .publications
        .get_mut(stream::AUDIO)
        .unwrap()
        .revision = publication::MAX_REVISION;
    let mut events = store.subscribe_domains(&[stream::AUDIO]);
    for percent in [1, 2] {
        store
            .update_audio(AudioState {
                volume_percent: percent,
                ..Default::default()
            })
            .await;
    }
    let event = events.try_recv().unwrap();
    assert_eq!(event.data["_publication"]["previous_epoch"], old_epoch);
    assert_eq!(event.data["_publication"]["revision"], 2);
    assert_ne!(event.data["_publication"]["epoch"], old_epoch);
    assert_ne!(
        StateStore::default().snapshot().await.publications[stream::AUDIO].epoch,
        old_epoch
    );
}

#[tokio::test]
async fn bounded_transition_lag_recovers_current_state_not_missing_edges() {
    let store = StateStore::default();
    let mut events = store.subscribe_domains(&[stream::BATTERY]);
    for n in 0..300 {
        store
            .update_battery(BatteryState {
                percentage: (n % 100) as u8,
                ..Default::default()
            })
            .await;
    }
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
    ));
    let (baseline, _) = store
        .snapshot_and_subscribe_domains(&[stream::BATTERY])
        .await;
    let current =
        read_wire::Event::subscribed(stream::BATTERY, "new-subscription", &baseline).into_value();
    assert_eq!(current["event"], "subscribed");
    assert_eq!(current["data"]["percentage"], 99);
    assert_eq!(
        current["data"]["_publication"],
        serde_json::to_value(&baseline.publications[stream::BATTERY]).unwrap()
    );
}
