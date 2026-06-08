mod runtime;
mod scenarios;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    phoxal::runtime::execute::<runtime::FrameRuntime>().await
}
