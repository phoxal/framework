//! Long-lived Webots world-session host.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::attachment::WebotsAttachments;
use crate::evidence::{EvidenceSession, world_terminal_summary};
use crate::generation::{ControllerExecutables, stage_project};
use crate::lifecycle::{
    LogCaptureOutcome, NativeProcessIdentity, WebotsInstallation, WebotsProcess,
};
use crate::registration::{
    EVIDENCE_DIRECTORY_ENV, LOG_BYTE_LIMIT_ENV, REGISTRY_DIRECTORY_ENV, RegistrationGuard,
    current_process_identity,
};
use crate::runtime::{WebotsWorldSession, WorldRuntime};
use crate::server::HostServer;
use crate::state::{NativeWorldFailure, NativeWorldLifecycle};
use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use phoxal::bundle::WorldBundle;
use phoxal::model::world::WorldInstanceId;
use phoxal::supervisor::api::simulation::SimulationEndReason;
use phoxal::world::WorldSessionServer;
use phoxal::world::api::session::document::{
    TerminalCleanup, TerminalFailure, TerminalOutcome, TerminalRetention,
};
use phoxal::world::api::session::{WorldLifecycle, WorldMember, WorldMemberPhase};
use tracing_subscriber::EnvFilter;

const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(30);
const WORLD_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const RECONCILE_INTERVAL: Duration = Duration::from_millis(5);

mod application;
mod assets;
mod attachment;
mod evidence;
mod generation;
mod glb;
mod lifecycle;
mod logging;
mod obj;
mod plan;
mod registration;
mod robot_generation;
mod runtime;
mod server;
mod shutdown;
mod state;

/// The exact native controller executable names generated into a Webots project.
const WORLD_CONTROLLER_PACKAGE: &str = "phoxal-simulator-webots-world-controller";
const ROBOT_CONTROLLER_PACKAGE: &str = "phoxal-simulator-webots-robot-controller";

use application::run;
use logging::{BoundedStderr, required_log_limit};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Canonical compiled WorldBundle directory.
    #[arg(long, value_name = "PATH")]
    world_bundle: PathBuf,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let log_byte_limit = match required_log_limit() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("webots host configuration failed: {error:#}");
            std::process::exit(2);
        }
    };
    let host_log_limit = (log_byte_limit / 2).max(1);
    let host_log = BoundedStderr::new(host_log_limit);
    let host_log_observer = host_log.clone();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(move || host_log.clone())
        .init();
    if let Err(error) = run(args, log_byte_limit, host_log_observer).await {
        tracing::error!(error = %format!("{error:#}"), "Webots world host failed");
        std::process::exit(1);
    }
}
