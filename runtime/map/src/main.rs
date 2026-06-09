mod core;
mod runtime;
mod scenarios;
mod selector;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    phoxal::runtime::execute::<runtime::MapRuntime>().await
}
