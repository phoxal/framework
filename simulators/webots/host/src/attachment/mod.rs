//! Serialized robot admission, native import, and supervisor commit.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use phoxal::identity::ExecutionId;
use phoxal::model::identity::SpawnId;
use phoxal::model::world::{World, WorldInstanceId};
use phoxal::simulator::{SimulationHostConnectOptions, SimulationHostSession};
use phoxal::supervisor::api::simulation::SimulationEndReason;
use phoxal::supervisor::api::simulation::attach::AttachRequest;
use phoxal::world::api::session::WorldMember;
use phoxal::world::api::session::WorldMemberPhase;
use phoxal::world::api::session::state::WorldSessionState;
use phoxal::world::api::session::{WorldMemberCleanup, WorldMemberEndReason, WorldMemberTerminal};
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::assets::StagedRobotAssets;
use crate::evidence::{EvidenceSession, world_member_evidence};
use crate::plan::{lower_robot_plan, required_assets};
use crate::robot_generation::{render_robot, robot_definition};
use crate::runtime::WorldRuntime;
use crate::server::HostServer;
use crate::state::{NativeRobotFailure, NativeWorldLifecycle};
use phoxal_simulator_webots_shared::protocol::validate_robot_import;

const CONTROLLER_READY_TIMEOUT: Duration = Duration::from_secs(30);

mod preparation;
mod removal;
mod transaction;
mod workers;

use preparation::{
    PreparedRobot, ensure_attach_slot, ensure_idempotent_request, prepare_robot, resolve_spawn,
    wait_for_active_ack, wait_for_controller,
};
use workers::{AttachmentWorkers, CancelOnDrop, OperationCancellation};

/// Concrete attachment authority retained by one world session.
#[derive(Clone)]
pub struct WebotsAttachments {
    pub(super) instance: WorldInstanceId,
    pub(super) world: World,
    pub(super) project_root: PathBuf,
    pub(super) native: Arc<HostServer>,
    pub(super) evidence: Arc<EvidenceSession>,
    pub(super) sessions: Arc<Mutex<BTreeMap<String, AttachedSession>>>,
    pub(super) workers: Arc<Mutex<AttachmentWorkers>>,
}

pub(super) struct AttachedSession {
    #[allow(
        dead_code,
        reason = "retaining the host session retains source-bound liveness"
    )]
    pub(super) host: SimulationHostSession,
    pub(super) definition: String,
    pub(super) member: WorldMember,
    pub(super) supervisor_endpoint: String,
    pub(super) assets: StagedRobotAssets,
}

impl WebotsAttachments {
    #[must_use]
    pub fn new(
        instance: WorldInstanceId,
        world: World,
        project_root: PathBuf,
        native: Arc<HostServer>,
        evidence: Arc<EvidenceSession>,
    ) -> Self {
        Self {
            instance,
            world,
            project_root,
            native,
            evidence,
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            workers: Arc::new(Mutex::new(AttachmentWorkers::new())),
        }
    }
}

#[cfg(test)]
mod tests;
