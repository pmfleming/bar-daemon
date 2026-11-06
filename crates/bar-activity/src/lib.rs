//! Static activity service assembly. Native bindings belong only to this owner's providers.
/// Run the resident owner.
pub async fn run() -> anyhow::Result<()> {
    bar_daemon_core::run_activity_daemon(bar_daemon_core::NativeProviders::default()).await
}
/// Run the daemon/client command line for this owner.
pub async fn main() -> anyhow::Result<()> {
    bar_daemon_core::component::main(bar_daemon_core::component::Component::Activity, run).await
}
