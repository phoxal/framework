//! Per-Robot Webots controller and narrow simulator-SDK bridge.

#[cfg(any(target_env = "musl", all(target_os = "linux", target_arch = "aarch64")))]
compile_error!(
    "the Webots R2025a controller SDK is dynamically linked and unsupported on musl or Linux aarch64"
);

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use phoxal::SampleSchedule;
use phoxal::api;
use phoxal::bus::{FixedSourceLease, LeaseDecision, LeaseRejection, ParticipantReadyEvents};
use phoxal::drive::authority::DriveCommandAuthority;
use phoxal::identity::ParticipantId;
use phoxal::model::component::capability::{
    Capability as DeclaredCapability, CapabilityKind, MotorCommand,
};
use phoxal::model::world::{WorldProgress, WorldProgressError};
use phoxal::simulation::api::step::StepEvent;
use phoxal::simulator::{
    ActiveBoundaryStamp, LiveSamplePublisher, LiveSetpointReceiver, LiveTransitionStamp,
};
use phoxal::simulator::{SimulatorConnectOptions, SimulatorError, SimulatorSession};
use phoxal::supervisor::api::simulation::SimulationAttachmentPhase;
use phoxal_simulator_webots_shared::plan::{
    CapabilityBinding as PlannedBinding, RobotSimulationPlan,
};
use phoxal_simulator_webots_shared::protocol::{
    ActuationDecision, ActuationEvidence, ActuationSelection, AppliedActuation, ControllerEvent,
    ControllerFault, ControllerLink, ControllerRole, HostDirective, NativeMotion,
    NoActuationReason, OfferedActuation,
};
use tracing_subscriber::EnvFilter;
use webots_rs::Webots;

mod actuation_evidence;
mod devices;
mod parking;
mod runtime;
mod sensors;

use sensors::SensorSet;

use actuation_evidence::{PendingActuationEvidence, evidence_decision};
use devices::DeviceSet;
use parking::{PARKED_POLL, park_after_cooperative_failure};
use runtime::{observed_progress, run, synchronize_devices};

#[cfg(test)]
use devices::motor::{MotorAction, classify_selection, dispatch_motor, stop_every};
#[cfg(test)]
use runtime::{
    ControllerLoopExit, activation_progress, authority_exit, publish_completed_transition,
};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Supervisor endpoint identifying exactly one robot execution.
    #[arg(long, value_name = "SUPERVISOR_ENDPOINT")]
    connect: String,
    /// Loopback-only endpoint owned by the world-session host.
    #[arg(long, value_name = "LOCAL_ENDPOINT")]
    host_connect: String,
}

// Zenoh requires a multi-thread runtime. Tokio drives this root future on the calling
// thread, keeping every Webots SDK call on the controller's native main thread.
#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    run(Args::parse()).await
}

#[cfg(test)]
mod tests;
