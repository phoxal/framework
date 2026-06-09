mod core;
mod runtime;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    phoxal::runtime::execute::<runtime::PerceptionRuntime>().await
}
