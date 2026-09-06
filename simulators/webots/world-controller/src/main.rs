//! Shared Webots supervisor controller for one Phoxal world session.

#[cfg(any(target_env = "musl", all(target_os = "linux", target_arch = "aarch64")))]
compile_error!(
    "the Webots R2025a controller SDK is dynamically linked and unsupported on musl or Linux aarch64"
);

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use phoxal_simulator_webots_shared::protocol::{
    ControllerEvent, ControllerFault, ControllerLink, ControllerRole, HostDirective, NativeMotion,
    NativeMutation, NativeProgressObservation, ObservedNativeMode,
};
use tracing_subscriber::EnvFilter;
use webots_rs::bindings::{
    WbSimulationMode, WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_FAST,
    WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_PAUSE,
    WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_REAL_TIME,
};
use webots_rs::{
    Webots,
    supervisor::{Node, Supervisor},
};

mod mode;
mod mutation;
mod runtime;

use mode::{
    observed_mode, poll_while_paused, set_motion, synchronize_control, validate_native_mode,
};
use mutation::{apply_mutation, start_imported_controller};
use runtime::run;

#[cfg(test)]
use runtime::{converge_on_error, exact_step_ms, observed_elapsed_ns};

const PAUSED_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Loopback-only endpoint owned by the world-session host.
    #[arg(long, value_name = "LOCAL_ENDPOINT")]
    host_connect: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    run(Args::parse())
}

#[cfg(test)]
mod tests;
