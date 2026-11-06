#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bar_compositor::main().await
}
