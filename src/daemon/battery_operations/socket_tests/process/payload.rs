//! Actual bar default framing bounds using synthetic typed battery state only.
use super::*;

async fn padded(peer: &mut Peer, value: Value, bytes: usize) -> Result<()> {
    let mut prefix = value.to_string();
    ensure!(prefix.pop() == Some('}'), "fixture object");
    prefix.push_str(",\"fixture_padding\":\"");
    let suffix = "\"}\n";
    let padding = bytes
        .checked_sub(prefix.len() + suffix.len() - 1)
        .context("padding bound")?;
    timeout(DEADLINE, async {
        peer.io.get_mut().write_all(prefix.as_bytes()).await?;
        let chunk = vec![b'x'; 64 * 1024];
        let mut remaining = padding;
        while remaining > 0 {
            let count = remaining.min(chunk.len());
            peer.io.get_mut().write_all(&chunk[..count]).await?;
            remaining -= count;
        }
        peer.io.get_mut().write_all(suffix.as_bytes()).await
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn default_request_ceiling_rejects_fake_effect_before_dispatch_and_resynchronizes()
-> Result<()> {
    timeout(Duration::from_secs(30), async {
        let directory = root()?;
        let root = std::fs::canonicalize(directory.path())?;
        let mut process = Process::spawn(&root, 1)?;
        let mut a = process.peer().await?;
        let mut b = process.peer().await?;
        let limit = JsonlSocketConfig::default().maximum_request_bytes;
        ensure!(limit == 1024 * 1024, "review changed bar production request ceiling");
        padded(&mut a, json!({"id":"maximum", "op":"call", "method":"bar.snapshot", "params":{"streams":["power-sleep.changed"]}}), limit).await?;
        let record = a.record().await?;
        ensure!(record["id"] == "maximum" && record["response"]["ok"] == true, "exact request ceiling failed");
        padded(&mut a, json!({"id":"must-not-start", "op":"call", "method":"fixture.battery.start", "params":{"case":"accepted"}}), limit + 1).await?;
        let rejected = a.record().await?;
        ensure!(rejected["kind"] == "protocol-error" && rejected["error"].as_str().is_some_and(|e| e.contains("exceeds")), "one-byte-over request not rejected");
        snapshot(&mut a).await?; // parser resynchronizes after the rejected LF
        snapshot(&mut b).await?;
        counts(&mut b, 0, 0, 0).await?;
        ensure!(!root.join("audit").exists(), "oversized fake effect reached coordinator work");
        drop(a);
        drop(b);
        process.kill().await?;
        println!("BAR_RUNTIME_REQUEST_PASS bytes={limit} over={}", limit + 1);
        Ok::<_, anyhow::Error>(())
    }).await?
}

async fn finish_after_owner_loss(root: &Path, peer: &mut Peer, id: &str) -> Result<()> {
    marker(root, "cancelled:accepted").await?;
    counts(peer, 1, 0, 0).await?;
    ensure!(
        peer.call("fixture.battery.start", json!({"case":"fresh"}))
            .await?["error"]["code"]
            == "operation-active",
        "output/caller loss discarded accepted admission"
    );
    ensure!(
        peer.call("battery.operation.status", json!({"operation_id":id}))
            .await?["error"]["code"]
            == "operation-not-found",
        "output/caller loss transferred owner authority"
    );
    let healthy = peer
        .call("bar.snapshot", json!({"streams":["power-sleep.changed"]}))
        .await?;
    ensure!(healthy["ok"] == true, "output failure retired healthy peer");
    peer.call("fixture.reconcile", json!({})).await?;
    counts(peer, 0, 0, 0).await?;
    let audit = std::fs::read_to_string(root.join("audit"))?;
    ensure!(
        audit
            .lines()
            .filter(|line| line.starts_with("started:"))
            .count()
            == 1
            && audit.contains("settled:accepted"),
        "failed output replayed work or lost reconciliation"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "explicit 16 MiB private bar service-result qualification"]
async fn maximum_service_result_and_unknown_overflow_keep_owner_and_admission() -> Result<()> {
    timeout(Duration::from_secs(60), async {
        // ServiceBoundary::new limits the API JSON, independently of the 32 MiB
        // socket frame. Do not raise/bypass that boundary to reach the writer.
        let limit = 16 * 1024 * 1024;
        for oversized in [false, true] {
            let directory = root()?;
            let root = std::fs::canonicalize(directory.path())?;
            let mut process = Process::spawn(&root, 1)?;
            let mut a = process.peer().await?;
            let mut b = process.peer().await?;
            let id = a.start_battery("accepted").await?;
            marker(&root, "started:accepted").await?;
            b.call("fixture.payload", json!({"bytes":0})).await?;
            let small = a
                .call("bar.snapshot", json!({"streams":["battery.changed"]}))
                .await?;
            ensure!(
                small["data"]["snapshot"]["battery"]["error"] == "",
                "typed fixture error field"
            );
            let overhead = serde_json::to_vec(&small)?.len();
            let bytes = limit - overhead + usize::from(oversized);
            b.call("fixture.payload", json!({"bytes":bytes})).await?;
            let response = a
                .call("bar.snapshot", json!({"streams":["battery.changed"]}))
                .await?;
            if oversized {
                ensure!(
                    response["ok"] == false && response["error"]["code"] == "outcome-unknown",
                    "service overflow did not remain unknown"
                );
                ensure!(
                    serde_json::to_vec(&response)?.len() < 1024,
                    "overflow reply unbounded"
                );
            } else {
                ensure!(
                    response["ok"] == true && serde_json::to_vec(&response)?.len() == limit,
                    "exact service result ceiling failed"
                );
                ensure!(
                    response["data"]["snapshot"]["battery"]["error"]
                        .as_str()
                        .map(str::len)
                        == Some(bytes),
                    "typed result truncated"
                );
            }
            let status = a
                .call("battery.operation.status", json!({"operation_id":id}))
                .await?;
            ensure!(
                status["data"]["operation"]["state"] == "running"
                    && status["data"]["operation"]["cancellation_requested"] == false,
                "bounded service result retired owner or its work"
            );
            counts(&mut b, 1, 0, 0).await?;
            drop(a);
            finish_after_owner_loss(&root, &mut b, &id).await?;
            drop(b);
            process.kill().await?;
            println!(
                "BAR_RUNTIME_SERVICE_RESULT_PASS api_bytes={} oversized={oversized}",
                limit + usize::from(oversized)
            );
        }
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
#[ignore = "explicit 32 MiB private bar event framing/backpressure qualification"]
async fn maximum_event_and_failed_writer_hold_battery_admission_until_reconciled() -> Result<()> {
    timeout(Duration::from_secs(90), async {
        let limit = JsonlSocketConfig::default().maximum_response_bytes;
        ensure!(
            limit == 32 * 1024 * 1024,
            "review changed bar production writer ceiling"
        );
        for case in ["maximum", "oversized", "stalled"] {
            let directory = root()?;
            let root = std::fs::canonicalize(directory.path())?;
            if case == "stalled" {
                std::fs::write(root.join("short-write-deadline"), b"fixture only\n")?;
            }
            let mut process = Process::spawn(&root, 1)?;
            let mut a = process.peer().await?;
            let mut b = process.peer().await?;
            let id = a.start_battery("accepted").await?;
            marker(&root, "started:accepted").await?;
            a.request(json!({"op":"subscribe", "streams":["battery.changed"]}))
                .await?;
            let initial = if a.events.is_empty() {
                a.record().await?
            } else {
                a.events.remove(0)
            };
            ensure!(
                initial["event"]["event"] == "subscribed",
                "initial baseline missing"
            );
            b.call("fixture.payload", json!({"bytes":0})).await?;
            let small = a.record().await?;
            ensure!(
                small["kind"] == "event"
                    && small["event"]["event"] == "changed"
                    && small["event"]["data"]["error"] == "",
                "typed fixture event missing"
            );
            let overhead = serde_json::to_vec(&small)?.len() + 1; // writer includes LF
            let bytes = limit - overhead + usize::from(case == "oversized");
            b.call("fixture.payload", json!({"bytes":bytes})).await?;
            if case == "maximum" {
                let mut line = String::new();
                let read = timeout(Duration::from_secs(15), a.io.read_line(&mut line)).await??;
                ensure!(
                    read == limit && line.len() == limit,
                    "exact event ceiling changed: {read} expected {limit}"
                );
                let record: Value = serde_json::from_str(&line)?;
                ensure!(
                    record["kind"] == "event"
                        && record["event"]["event"] == "changed"
                        && record["event"]["data"]["error"].as_str().map(str::len) == Some(bytes),
                    "maximum typed event failed"
                );
                drop(a);
            } else if case == "oversized" {
                // The real subscription bypasses no API policy: event delivery
                // has its own writer bound. No partial frame may be emitted.
                let mut line = String::new();
                ensure!(
                    timeout(DEADLINE, a.io.read_line(&mut line)).await?? == 0 && line.is_empty(),
                    "oversized event emitted a partial or successful frame"
                );
                drop(a);
            } else {
                // DO NOT read: real kernel backpressure, not a mock writer.
                // 100 ms is fixture-only, not production timing evidence.
                marker(&root, "cancelled:accepted").await?;
                drop(a);
            }
            finish_after_owner_loss(&root, &mut b, &id).await?;
            drop(b);
            process.kill().await?;
            println!(
                "BAR_RUNTIME_EVENT_PASS case={case} frame_bytes={}",
                limit + usize::from(case == "oversized")
            );
        }
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
