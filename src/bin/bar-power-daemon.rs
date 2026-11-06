#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bar_daemon::component::main(bar_daemon::component::Component::Power).await
}
