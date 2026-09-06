//! Backend-neutral world-session projection over validated native Webots state.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use phoxal::bundle::WorldBundle;
use phoxal::identity::ExecutionId;
use phoxal::model::world::{WorldInstanceId, WorldProgress, WorldProvenance};
use phoxal::supervisor::api::simulation::SimulationEndReason;
use phoxal::version::FrameworkVersion;
use phoxal::world::api::session::connect::WorldSessionBootstrap;
use phoxal::world::api::session::control::WorldControl;
use phoxal::world::api::session::diagnostics::{ObservedWorldPacing, WorldSessionDiagnostics};
use phoxal::world::api::session::document::WorldCheckpoint;
use phoxal::world::api::session::state::WorldSessionState;
use phoxal::world::api::session::{WorldLifecycle, WorldMotion};
use tokio::sync::broadcast;

use crate::evidence::{EvidenceSession, world_checkpoint};
use crate::registration::ProcessIdentity;
use crate::server::HostServer;
use crate::state::{NativeWorldFailure, NativeWorldLifecycle, NativeWorldState};
use phoxal_simulator_webots_shared::protocol::NativeMotion;

const STREAM_CAPACITY: usize = 64;
const PACING_WINDOW_TRANSITIONS: usize = 128;
const DIAGNOSTICS_EMISSION_INTERVAL: Duration = Duration::from_secs(1);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

mod checkpoint;
mod control;
mod handler;
mod pacing;
mod projection;

pub use handler::WebotsWorldSession;
pub use projection::WorldRuntime;

use checkpoint::CheckpointWriter;
use pacing::{DiagnosticsState, clear_pacing_state, project_diagnostics, record_pacing};
use projection::{lock, next_revision};
