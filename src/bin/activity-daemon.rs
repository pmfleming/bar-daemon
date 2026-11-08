#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bar_activity::main().await
}
