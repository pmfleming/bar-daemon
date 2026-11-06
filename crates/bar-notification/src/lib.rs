//! Static notification service assembly. Native bindings belong only to this owner's providers.
/// Run the resident owner.
pub async fn run() -> anyhow::Result<()> {
    bar_daemon_core::run_notification_daemon(bar_daemon_core::NativeProviders {
        sharing: Some(bar_sharing_native::observe),
        ..Default::default()
    })
    .await
}
/// Run the daemon/client command line for this owner.
pub async fn main() -> anyhow::Result<()> {
    bar_daemon_core::component::main(bar_daemon_core::component::Component::Notifications, run)
        .await
}
