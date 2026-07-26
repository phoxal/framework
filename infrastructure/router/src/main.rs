use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "phoxal-infrastructure-router",
    about = "Run the Phoxal-owned Zenoh router"
)]
struct Args {
    /// Native Zenoh JSON5 configuration file.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Zenoh endpoints on which this router listens.
    #[arg(long, value_name = "ENDPOINT", required = true)]
    listen: Vec<String>,
}

#[tokio::main]
async fn main() -> zenoh::Result<()> {
    let args = Args::parse();
    let mut config = args
        .config
        .map(zenoh::Config::from_file)
        .transpose()?
        .unwrap_or_default();
    config.insert_json5("mode", r#""router""#)?;
    config.insert_json5("listen/endpoints", &serde_json::to_string(&args.listen)?)?;
    let _router = zenoh::open(config).await?;
    std::future::pending::<()>().await;
    Ok(())
}
