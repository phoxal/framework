mod core;
mod range_classification;
mod runtime;
mod scenarios;
mod selector;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    phoxal::runtime::execute::<runtime::SafetyRuntime>().await
}
