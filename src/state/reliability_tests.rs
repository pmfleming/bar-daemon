use super::*;
use std::time::{Duration, Instant};

#[tokio::test]
async fn measured_domain_delivery_baseline() {
    let audio = Source::new(crate::protocol::stream::AUDIO);
    let battery = Source::new(crate::protocol::stream::BATTERY);
    let mut receiver = audio.subscribe();
    let mut unaffected = battery.subscribe();
    let mut latencies = Vec::new();
    let mut state = crate::model::AudioState::default();
    for index in 0..2000 {
        state.volume_percent = (index % 101) as u8;
        let event = DomainEvent {
            stream: crate::protocol::stream::AUDIO.into(),
            data: serde_json::to_value(&state).unwrap(),
        };
        let started = Instant::now();
        audio.send(event);
        receiver.recv().await.unwrap();
        latencies.push(started.elapsed().as_secs_f64() * 1_000_000.0);
    }
    let (old, mut baseline) = broadcast::channel(128);
    for _ in 0..1024 {
        let event = DomainEvent {
            stream: crate::protocol::stream::AUDIO.into(),
            data: serde_json::to_value(&state).unwrap(),
        };
        old.send(event.clone()).unwrap();
        audio.send(event);
    }
    assert!(matches!(
        baseline.try_recv(),
        Err(TryRecvError::Lagged(896))
    ));
    receiver.recv().await.unwrap();
    assert!(matches!(unaffected.try_recv(), Err(TryRecvError::Empty)));
    let idle_received = tokio::time::timeout(Duration::from_millis(100), receiver.recv())
        .await
        .is_ok();
    assert!(!idle_received);
    latencies.sort_by(f64::total_cmp);
    println!(
        "SHELLLIST_RELIABILITY {}",
        serde_json::json!({
            "scenario":"bar-domain-source-not-end-to-end", "samples":latencies.len(),
            "latency_us":{"p50":latencies[999],"p95":latencies[1899],"p99":latencies[1979]},
            "publications":audio.metrics.events.load(Ordering::Relaxed),
            "payload_bytes":audio.metrics.bytes.load(Ordering::Relaxed),
            "coalesced":audio.metrics.coalesced.load(Ordering::Relaxed),
            "lagged":audio.metrics.lag.load(Ordering::Relaxed),
            "old_ring_burst_dropped":896,"unaffected_domain_events":0,"idle_events":0,"idle_ms":100
        })
    );
}
