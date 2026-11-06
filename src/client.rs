use anyhow::Result;
use shelllist_daemon_core::DaemonEndpoint;
use shelllist_daemon_tokio::{BasicCorrelation, JsonlClientConfig, run_jsonl_client};

use crate::api::{INTERFACE, OBJECT_PATH};

pub(crate) async fn run() -> Result<()> {
    run_component(crate::component::Component::Bar).await
}

pub(crate) async fn run_component(component: crate::component::Component) -> Result<()> {
    run_jsonl_client(JsonlClientConfig {
        endpoint: DaemonEndpoint::new(
            component.name(),
            component.bus_name(),
            OBJECT_PATH,
            INTERFACE,
        ),
        correlation: BasicCorrelation,
        pending_event_limit: 32,
        max_in_flight_requests: 64,
        shutdown_timeout: Some(std::time::Duration::from_secs(5)),
    })
    .await
}
