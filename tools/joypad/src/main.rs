mod args;
mod backend;
mod mapping;
mod model;
mod runtime;
mod selection;

use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use phoxal::bus::builder::Builder;
use phoxal::bus::query::Retry;
use phoxal::runtime::RuntimeProcess;
use phoxal::util::init_tracing;
use tracing::info;

use crate::args::Args;
use crate::backend::Backend;
use crate::model::fetch_control_scheme;
use crate::selection::select_device;

const CLOCK_PERIOD: Duration = Duration::from_millis(20);

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;
    let args = Args::parse();
    let selected_device = select_device(Backend::new()?, args.controller).await?;
    let bus = Builder::new(args.router_endpoint)
        .with_connect_timeout(Duration::from_millis(args.robot_connect_timeout_ms))
        .with_connect_retries(args.robot_connect_retries)
        .with_prefix(args.robot_namespace)
        .connect()
        .await?;
    let retry = Retry::new(args.robot_connect_retries.saturating_add(1));
    let scheme = fetch_control_scheme(&bus, &retry).await?;

    info!(
        device_name = %selected_device.name,
        device_uuid = %selected_device.uuid_hyphenated(),
        scheme = ?scheme,
        "Joypad runtime ready"
    );

    RuntimeProcess::new(&bus, args.simulation, CLOCK_PERIOD)
        .run::<runtime::JoypadRuntime>(runtime::Config {
            selected_device,
            scheme,
        })
        .await?;

    Ok(())
}
