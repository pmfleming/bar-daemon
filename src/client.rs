use anyhow::Result;
use shelllist_daemon_core::DaemonEndpoint;
use shelllist_daemon_tokio::{BasicCorrelation, JsonlClientConfig, run_jsonl_client};

use crate::api::{BUS_NAME, INTERFACE, OBJECT_PATH};

const ENDPOINT: DaemonEndpoint =
    DaemonEndpoint::new("bar-daemon", BUS_NAME, OBJECT_PATH, INTERFACE);

pub(crate) async fn run() -> Result<()> {
    run_jsonl_client(JsonlClientConfig {
        endpoint: ENDPOINT,
        correlation: BasicCorrelation,
        pending_event_limit: 32,
        max_in_flight_requests: 64,
        shutdown_timeout: Some(std::time::Duration::from_secs(5)),
    })
    .await
}
