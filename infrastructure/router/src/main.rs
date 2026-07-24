#[tokio::main]
async fn main() -> anyhow::Result<()> {
    phoxal_infrastructure_router::run().await
}
