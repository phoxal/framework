use phoxal_runtime_localize::runtime;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    phoxal::runtime::execute::<runtime::LocalizeRuntime>().await
}
