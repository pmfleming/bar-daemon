//! Static status service assembly. Native bindings belong only to this owner's providers.
/// Run the resident owner.
pub async fn run() -> anyhow::Result<()> {
    bar_daemon_core::run_daemon(bar_daemon_core::NativeProviders {
        audio: Some(std::sync::Arc::new(bar_audio_native::Provider)),
        ..Default::default()
    })
    .await
}
/// Run the daemon/client command line for this owner.
pub async fn main() -> anyhow::Result<()> {
    bar_daemon_core::component::main(bar_daemon_core::component::Component::Bar, run).await
}
