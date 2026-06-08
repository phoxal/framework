mod frontiers;
mod runtime;
mod scenarios;
mod scoring;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    phoxal::runtime::execute::<runtime::ExploreRuntime>().await
}
