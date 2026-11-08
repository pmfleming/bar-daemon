#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bar_notification::main().await
}
