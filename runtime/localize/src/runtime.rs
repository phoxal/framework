use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use phoxal::api::component::v1::capability::{camera, depth, gnss, imu};
use phoxal::api::frame::v1::FrameId;
use phoxal::api::localize::v1::{
    AffectedKeyframeSummary, CorrectionsRequest, CorrectionsResponse, Covariance, ImuBiasEstimate,
    Keyframe, KeyframeRequest, KeyframeResponse, LocalizationMode, LocalizationRevision,
    LocalizationRevisionCause, LocalizationRevisionId, LocalizationSource, LocalizationState,
    LocalizationStatus, LocalizationStatusReason, PoseEstimate, PoseGraphCorrection,
    PoseGraphRequest, PoseGraphResponse, VelocityEstimate,
};
use phoxal::api::odometry::v1::{OdometryEstimate, StatusMode};
use phoxal::api::simulation::v1::pose::Pose as SimPose;
use phoxal::api::v1::topic;
use phoxal::bus::typed::Received;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::GnssCoordinateSystem;
use phoxal::runtime::clock::Step;
use phoxal::runtime::decision_log::DecisionLog;
use phoxal::runtime::{EmptyArgs, QueryOptions, ReadCell, RobotRuntimeArgs};
use phoxal::runtime::{Io, Runtime, RuntimeInputs, TopicPublisher};

use crate::gnss_anchored::GnssAnchoredBackend;
use crate::orbslam3;
use crate::selector::{self, ENV_ORB_SLAM3_VOCABULARY};
use crate::sim_truth::SimulatorTruthBackend;

const CLOCK_PERIOD: Duration = Duration::from_millis(20);
const ENV_LOCALIZE_BACKEND: &str = "ROBOT_LOCALIZE_BACKEND";
pub(crate) const LOCALIZE_EPOCH: u64 = 1;
const DEAD_RECKONING_READY_SAMPLES: u8 = 2;
const ODOM_FRAME_ID: &str = "odom";
const BASE_FRAME_ID: &str = "base_footprint";

#[derive(Clone)]
pub struct Config {
    backend: BackendSelection,
    clock_period: Duration,
}

impl Config {
    pub fn from_args(args: &RobotRuntimeArgs) -> Result<Self> {
        if args.simulation
            && std::env::var(ENV_LOCALIZE_BACKEND).ok().as_deref() == Some("simulator_truth")
        {
            return Ok(Self {
                backend: BackendSelection::SimulatorTruth {
                    robot_id: args.identity().robot_id,
                },
                clock_period: CLOCK_PERIOD,
            });
        }

        let robot = args.robot()?;
        let vocabulary_path = orb_slam3_vocabulary_from_env()?;
        Ok(Self {
            backend: selector::select_backend(&robot, vocabulary_path.as_deref())?,
            clock_period: CLOCK_PERIOD,
        })
    }

    pub const fn clock_period(&self) -> Duration {
        self.clock_period
    }
}

#[derive(Debug, Clone)]
#[allow(private_interfaces)]
pub enum BackendSelection {
    DeadReckoning,
    SimulatorTruth {
        robot_id: String,
    },
    GnssAnchored {
        gnss: CapabilityRef,
        coordinate_system: GnssCoordinateSystem,
    },
    OrbSlam3 {
        inputs: OrbSlam3Inputs,
        config: Box<orbslam3::OrbSlam3Config>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct OrbSlam3Inputs {
    pub(crate) camera: CapabilityRef,
    pub(crate) depth: CapabilityRef,
    pub(crate) imu: Option<CapabilityRef>,
}

fn orb_slam3_vocabulary_from_env() -> Result<Option<PathBuf>> {
    Ok(std::env::var_os(ENV_ORB_SLAM3_VOCABULARY).map(PathBuf::from))
}

#[async_trait::async_trait]
pub(crate) trait LocalizeBackend: Send {
    fn name(&self) -> LocalizationSource;

    fn ingest_odometry(&mut self, sample: Received<OdometryEstimate>);

    fn ingest_sim_pose(&mut self, _sample: Received<SimPose>) {}

    fn ingest_gnss(&mut self, _sample: Received<gnss::Sample>) {}

    fn ingest_imu(&mut self, _sample: Received<imu::Sample>) -> Result<()> {
        Ok(())
    }

    fn ingest_camera(&mut self, _sample: Received<camera::Frame>) -> Result<()> {
        Ok(())
    }

    fn ingest_depth(&mut self, _sample: Received<depth::Depth>) -> Result<()> {
        Ok(())
    }

    fn step(&mut self, step: Step) -> Result<BackendUpdate>;
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BackendUpdate {
    pub(crate) mode: LocalizationMode,
    pub(crate) pose: Option<PoseEstimate>,
    pub(crate) keyframe: Option<Keyframe>,
    pub(crate) velocity: Option<VelocityEstimate>,
    pub(crate) covariance: Option<Covariance>,
    pub(crate) imu_bias: Option<ImuBiasEstimate>,
    pub(crate) status: LocalizationStatus,
    pub(crate) valid_at_ns: Option<u64>,
    pub(crate) new_revision: Option<NewRevision>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NewRevision {
    pub(crate) cause: LocalizationRevisionCause,
    pub(crate) affected_keyframes: AffectedKeyframeSummary,
}

/// First time a backend reports `Tracking`, emit a one-shot initial revision so the
/// map/plan spatial chain activates. Returns `None` otherwise. Pure.
pub(crate) fn initial_sensor_integration_revision(
    mode: LocalizationMode,
    already_emitted: bool,
) -> Option<NewRevision> {
    if mode == LocalizationMode::Tracking && !already_emitted {
        Some(NewRevision {
            cause: LocalizationRevisionCause::SensorIntegration,
            affected_keyframes: AffectedKeyframeSummary {
                keyframe_ids: Vec::new(),
                region: None,
            },
        })
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DeadReckoningBackend {
    current_revision: LocalizationRevisionId,
    tracking_samples: u8,
    latest_odometry: Option<Received<OdometryEstimate>>,
    initial_revision_emitted: bool,
}

impl Default for DeadReckoningBackend {
    fn default() -> Self {
        Self {
            current_revision: current_revision(),
            tracking_samples: 0,
            latest_odometry: None,
            initial_revision_emitted: false,
        }
    }
}

#[async_trait::async_trait]
impl LocalizeBackend for DeadReckoningBackend {
    fn name(&self) -> LocalizationSource {
        LocalizationSource::DeadReckoning
    }

    fn ingest_odometry(&mut self, sample: Received<OdometryEstimate>) {
        if sample.value.status.mode == StatusMode::Tracking {
            self.tracking_samples = self
                .tracking_samples
                .saturating_add(1)
                .min(DEAD_RECKONING_READY_SAMPLES);
        }
        self.latest_odometry = Some(sample);
    }

    fn step(&mut self, step: Step) -> Result<BackendUpdate> {
        let Some(sample) = &self.latest_odometry else {
            return Ok(BackendUpdate {
                mode: LocalizationMode::Initializing,
                pose: None,
                velocity: None,
                covariance: None,
                imu_bias: None,
                status: LocalizationStatus {
                    healthy: false,
                    reasons: vec![
                        LocalizationStatusReason::SensorMissing,
                        LocalizationStatusReason::BackendInitializing,
                    ],
                },
                valid_at_ns: None,
                new_revision: None,
                keyframe: None,
            });
        };

        let (mode, status) = match sample.value.status.mode {
            StatusMode::Tracking if self.tracking_samples >= DEAD_RECKONING_READY_SAMPLES => (
                LocalizationMode::DeadReckoning,
                LocalizationStatus {
                    healthy: true,
                    reasons: Vec::new(),
                },
            ),
            StatusMode::Tracking | StatusMode::Initializing => (
                LocalizationMode::Initializing,
                LocalizationStatus {
                    healthy: false,
                    reasons: vec![LocalizationStatusReason::BackendInitializing],
                },
            ),
            StatusMode::Degraded => (
                LocalizationMode::DeadReckoning,
                LocalizationStatus {
                    healthy: false,
                    reasons: vec![LocalizationStatusReason::SensorStale],
                },
            ),
            StatusMode::Stale => (
                LocalizationMode::Lost,
                LocalizationStatus {
                    healthy: false,
                    reasons: vec![LocalizationStatusReason::SensorStale],
                },
            ),
        };

        let new_revision =
            if mode == LocalizationMode::DeadReckoning && !self.initial_revision_emitted {
                self.initial_revision_emitted = true;
                Some(NewRevision {
                    cause: LocalizationRevisionCause::SensorIntegration,
                    affected_keyframes: AffectedKeyframeSummary {
                        keyframe_ids: Vec::new(),
                        region: None,
                    },
                })
            } else {
                None
            };

        Ok(BackendUpdate {
            mode,
            pose: Some(localize_pose_from_odometry(&sample.value.pose)),
            keyframe: None,
            velocity: Some(localize_velocity_from_odometry(&sample.value.velocity)),
            covariance: sample.value.covariance.as_ref().map(localize_covariance),
            imu_bias: None,
            status,
            valid_at_ns: Some(sample.at_ns.unwrap_or_else(|| step.tick.time_ns())),
            new_revision,
        })
    }
}

pub enum Input {
    Odometry(Received<OdometryEstimate>),
    SimPose(Received<SimPose>),
    Gnss(Received<gnss::Sample>),
    Imu(Received<imu::Sample>),
    Camera(Received<camera::Frame>),
    Depth(Received<depth::Depth>),
}

pub struct LocalizeView {
    current_revision: LocalizationRevisionId,
}

pub struct LocalizeRuntime {
    backend: Box<dyn LocalizeBackend>,
    view: ReadCell<LocalizeView>,
    current_revision: LocalizationRevisionId,
    revision_emitted: bool,
    decision_log: DecisionLog<LocalizeLogKey>,
    state_publisher: TopicPublisher<LocalizationState>,
    pose_publisher: TopicPublisher<PoseEstimate>,
    revision_publisher: TopicPublisher<LocalizationRevision>,
    keyframe_publisher: TopicPublisher<Keyframe>,
    _correction_publisher: TopicPublisher<PoseGraphCorrection>,
}

type LocalizeLogKey = (LocalizationMode, LocalizationSource);

#[async_trait::async_trait]
impl Runtime for LocalizeRuntime {
    const RUNTIME_ID: &'static str = "localize";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Config::from_args(common)
    }

    fn clock_period(config: &Self::Config) -> Duration {
        config.clock_period()
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        io.subscribe_topic(topic::new().v1().odometry().estimate(), Input::Odometry)
            .await?;
        if let BackendSelection::SimulatorTruth { robot_id } = &config.backend {
            io.subscribe_topic(
                topic::new()
                    .v1()
                    .simulation()
                    .robot(robot_id.clone())
                    .pose(),
                Input::SimPose,
            )
            .await?;
        }
        if let BackendSelection::GnssAnchored { gnss, .. } = &config.backend {
            io.subscribe_topic(
                topic::new()
                    .v1()
                    .component(gnss.component_id.clone())
                    .gnss(gnss.capability_id.clone())
                    .data(),
                Input::Gnss,
            )
            .await?;
        }
        if let BackendSelection::OrbSlam3 { inputs, .. } = &config.backend {
            if let Some(imu) = &inputs.imu {
                io.subscribe_topic(
                    topic::new()
                        .v1()
                        .component(imu.component_id.clone())
                        .imu(imu.capability_id.clone())
                        .data(),
                    Input::Imu,
                )
                .await?;
            }
            io.subscribe_topic(
                topic::new()
                    .v1()
                    .component(inputs.camera.component_id.clone())
                    .camera(inputs.camera.capability_id.clone())
                    .data(),
                Input::Camera,
            )
            .await?;
            io.subscribe_topic(
                topic::new()
                    .v1()
                    .component(inputs.depth.component_id.clone())
                    .depth(inputs.depth.capability_id.clone())
                    .data(),
                Input::Depth,
            )
            .await?;
        }
        let (current_revision, backend): (LocalizationRevisionId, Box<dyn LocalizeBackend>) =
            match config.backend {
                BackendSelection::DeadReckoning => {
                    let backend = DeadReckoningBackend::default();
                    (backend.current_revision, Box::new(backend))
                }
                BackendSelection::SimulatorTruth { .. } => {
                    (current_revision(), Box::new(SimulatorTruthBackend::new()))
                }
                BackendSelection::GnssAnchored {
                    coordinate_system, ..
                } => {
                    let backend = GnssAnchoredBackend::new(coordinate_system);
                    (current_revision(), Box::new(backend))
                }
                BackendSelection::OrbSlam3 { config, .. } => {
                    if phoxal_runtime_localize_orb_slam3_sys::LINKED {
                        let backend = orbslam3::OrbSlam3Backend::new(*config)?;
                        (current_revision(), Box::new(backend))
                    } else {
                        tracing::warn!(
                            "localization resolved to ORB-SLAM3 but this binary was built without the \
                             ORB-SLAM3 native library (ORB_SLAM3_DIR unset at build time); falling back to \
                             dead-reckoning"
                        );
                        let backend = DeadReckoningBackend::default();
                        (backend.current_revision, Box::new(backend))
                    }
                }
            };
        let view = ReadCell::new(LocalizeView { current_revision });

        io.serve_query_topic(
            topic::new().v1().localize().pose_graph(),
            view.reader(),
            QueryOptions::max_in_flight(NonZeroUsize::new(4).unwrap()),
            localize_pose_graph,
        )
        .await?;
        io.serve_query_topic(
            topic::new().v1().localize().keyframe_query(),
            view.reader(),
            QueryOptions::max_in_flight(NonZeroUsize::new(4).unwrap()),
            localize_keyframe,
        )
        .await?;
        io.serve_query_topic(
            topic::new().v1().localize().corrections(),
            view.reader(),
            QueryOptions::max_in_flight(NonZeroUsize::new(4).unwrap()),
            localize_corrections,
        )
        .await?;
        let state_topic = topic::new().v1().localize().state();

        Ok(Self {
            backend,
            view,
            current_revision,
            revision_emitted: false,
            decision_log: DecisionLog::from_topic(Self::RUNTIME_ID, &state_topic),
            state_publisher: io.publisher_topic(state_topic).await?,
            pose_publisher: io
                .publisher_topic(topic::new().v1().localize().pose())
                .await?,
            revision_publisher: io
                .publisher_topic(topic::new().v1().localize().revision())
                .await?,
            keyframe_publisher: io
                .publisher_topic(topic::new().v1().localize().keyframe())
                .await?,
            _correction_publisher: io
                .publisher_topic(topic::new().v1().localize().correction())
                .await?,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        for input in inputs {
            match input {
                Input::Odometry(sample) => self.backend.ingest_odometry(sample),
                Input::SimPose(sample) => self.backend.ingest_sim_pose(sample),
                Input::Gnss(sample) => self.backend.ingest_gnss(sample),
                Input::Imu(sample) => self.backend.ingest_imu(sample)?,
                Input::Camera(sample) => self.backend.ingest_camera(sample)?,
                Input::Depth(sample) => self.backend.ingest_depth(sample)?,
            }
        }

        let update = self.backend.step(step)?;
        let timestamp_ns = step.tick.time_ns();
        let state = LocalizationState {
            mode: update.mode,
            source: self.backend.name(),
            revision: Some(self.current_revision),
            pose: update.pose.clone(),
            velocity: update.velocity,
            covariance: update.covariance,
            imu_bias: update.imu_bias,
            status: update.status,
            valid_at_ns: update.valid_at_ns,
        };

        self.decision_log
            .observe(timestamp_ns, (state.mode, state.source));

        self.state_publisher.put(timestamp_ns, &state).await?;
        if let Some(pose) = update.pose {
            self.pose_publisher.put(timestamp_ns, &pose).await?;
        }
        if let Some(new_revision) = update.new_revision {
            let revision = publishable_revision(
                &mut self.current_revision,
                &mut self.revision_emitted,
                new_revision,
            );
            self.view.publish(LocalizeView {
                current_revision: self.current_revision,
            });
            self.revision_publisher.put(timestamp_ns, &revision).await?;
        }
        if let Some(keyframe) = update.keyframe {
            self.keyframe_publisher.put(timestamp_ns, &keyframe).await?;
        }

        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::ScenarioDescriptor] {
        crate::scenarios::SCENARIOS
    }

    async fn run_scenario(name: &str, common: &RobotRuntimeArgs, _args: &Self::Args) -> Result<()> {
        crate::scenarios::run(name, common).await
    }
}

pub(crate) fn current_revision() -> LocalizationRevisionId {
    LocalizationRevisionId {
        epoch: LOCALIZE_EPOCH,
        sequence: 0,
    }
}

pub(crate) fn publishable_revision(
    current_revision: &mut LocalizationRevisionId,
    revision_emitted: &mut bool,
    new_revision: NewRevision,
) -> LocalizationRevision {
    let previous_revision_id = if *revision_emitted {
        let previous = *current_revision;
        current_revision.sequence += 1;
        Some(previous)
    } else {
        None
    };
    *revision_emitted = true;

    LocalizationRevision {
        revision_id: *current_revision,
        previous_revision_id,
        cause: new_revision.cause,
        affected_keyframes: new_revision.affected_keyframes,
        inline_correction_available: false,
        correction_fetch_required: false,
    }
}

fn localize_pose_from_odometry(pose: &phoxal::api::odometry::v1::PoseEstimate) -> PoseEstimate {
    PoseEstimate {
        frame_id: FrameId::new(ODOM_FRAME_ID),
        child_frame_id: FrameId::new(BASE_FRAME_ID),
        translation_m: pose.translation_m,
        rotation_xyzw: pose.rotation_xyzw,
    }
}

fn localize_velocity_from_odometry(
    velocity: &phoxal::api::odometry::v1::VelocityEstimate,
) -> VelocityEstimate {
    VelocityEstimate {
        frame_id: velocity.frame_id.clone(),
        linear_mps: velocity.linear_mps,
        angular_radps: velocity.angular_radps,
    }
}

fn localize_covariance(covariance: &phoxal::api::odometry::v1::Covariance) -> Covariance {
    Covariance {
        values: covariance.values.clone(),
    }
}

pub(crate) fn pose_graph_response(
    request: &PoseGraphRequest,
    current: LocalizationRevisionId,
) -> PoseGraphResponse {
    if request.revision.epoch != current.epoch {
        return PoseGraphResponse::WrongEpoch { current };
    }
    PoseGraphResponse::RevisionUnavailable {
        latest_available: Some(current),
    }
}

pub(crate) fn keyframe_response(
    request: &KeyframeRequest,
    current: LocalizationRevisionId,
) -> KeyframeResponse {
    if request.revision.epoch != current.epoch {
        return KeyframeResponse::WrongEpoch { current };
    }
    KeyframeResponse::RevisionUnavailable {
        latest_available: Some(current),
    }
}

pub(crate) fn corrections_response(
    request: &CorrectionsRequest,
    current: LocalizationRevisionId,
) -> CorrectionsResponse {
    if request.from_revision.epoch != current.epoch || request.to_revision.epoch != current.epoch {
        return CorrectionsResponse::WrongEpoch { current };
    }
    CorrectionsResponse::RevisionUnavailable {
        latest_available: Some(current),
    }
}

fn localize_pose_graph(view: &LocalizeView, req: PoseGraphRequest) -> PoseGraphResponse {
    pose_graph_response(&req, view.current_revision)
}

fn localize_keyframe(view: &LocalizeView, req: KeyframeRequest) -> KeyframeResponse {
    keyframe_response(&req, view.current_revision)
}

fn localize_corrections(view: &LocalizeView, req: CorrectionsRequest) -> CorrectionsResponse {
    corrections_response(&req, view.current_revision)
}

#[cfg(test)]
mod tests {
    use phoxal::api::localize::v1::{KeyframeId, PoseGraphRange};
    use phoxal::api::odometry::v1::{
        Covariance as OdometryCovariance, PoseEstimate as OdometryPoseEstimate, Status,
        VelocityEstimate as OdometryVelocityEstimate,
    };
    use phoxal::api::simulation::v1::clock::Clock;
    use phoxal::runtime::clock::Step;

    use super::*;

    #[test]
    fn emits_initial_revision_on_first_tracking() {
        assert_eq!(
            initial_sensor_integration_revision(LocalizationMode::Tracking, false),
            Some(NewRevision {
                cause: LocalizationRevisionCause::SensorIntegration,
                affected_keyframes: AffectedKeyframeSummary {
                    keyframe_ids: Vec::new(),
                    region: None,
                },
            })
        );
    }

    #[test]
    fn does_not_re_emit_after_initial() {
        assert_eq!(
            initial_sensor_integration_revision(LocalizationMode::Tracking, true),
            None
        );
    }

    #[test]
    fn no_revision_when_not_tracking() {
        assert_eq!(
            initial_sensor_integration_revision(LocalizationMode::Initializing, false),
            None
        );
        assert_eq!(
            initial_sensor_integration_revision(LocalizationMode::Lost, false),
            None
        );
    }

    #[test]
    fn dead_reckoning_enters_dead_reckoning_after_two_tracking_samples() {
        let mut backend = DeadReckoningBackend::default();
        let step = step_at(20_000_000);

        backend.ingest_odometry(odometry_sample(1, StatusMode::Tracking));
        let first = step_backend(&mut backend, step);
        assert_eq!(first.mode, LocalizationMode::Initializing);

        backend.ingest_odometry(odometry_sample(2, StatusMode::Tracking));
        let second = step_backend(&mut backend, step);
        assert_eq!(second.mode, LocalizationMode::DeadReckoning);
    }

    #[test]
    fn dead_reckoning_emits_initial_revision_once() {
        let mut backend = DeadReckoningBackend::default();
        let step = step_at(20_000_000);

        backend.ingest_odometry(odometry_sample(1, StatusMode::Tracking));
        let first = step_backend(&mut backend, step);
        assert_eq!(first.new_revision, None);

        backend.ingest_odometry(odometry_sample(2, StatusMode::Tracking));
        let second = step_backend(&mut backend, step);
        assert_eq!(
            second.new_revision,
            Some(NewRevision {
                cause: LocalizationRevisionCause::SensorIntegration,
                affected_keyframes: AffectedKeyframeSummary {
                    keyframe_ids: Vec::new(),
                    region: None,
                },
            })
        );

        backend.ingest_odometry(odometry_sample(3, StatusMode::Tracking));
        let third = step_backend(&mut backend, step);
        assert_eq!(third.mode, LocalizationMode::DeadReckoning);
        assert_eq!(third.new_revision, None);
    }

    #[test]
    fn dead_reckoning_does_not_emit_revision_while_initializing() {
        let mut backend = DeadReckoningBackend::default();
        let step = step_at(20_000_000);

        let missing = step_backend(&mut backend, step);
        assert_eq!(missing.mode, LocalizationMode::Initializing);
        assert_eq!(missing.new_revision, None);

        backend.ingest_odometry(odometry_sample(1, StatusMode::Tracking));
        let first_tracking = step_backend(&mut backend, step);
        assert_eq!(first_tracking.mode, LocalizationMode::Initializing);
        assert_eq!(first_tracking.new_revision, None);
    }

    #[test]
    fn dead_reckoning_reports_lost_for_stale_odometry() {
        let mut backend = ready_backend();
        backend.ingest_odometry(odometry_sample(3, StatusMode::Stale));

        let update = step_backend(&mut backend, step_at(30_000_000));

        assert_eq!(update.mode, LocalizationMode::Lost);
    }

    #[test]
    fn dead_reckoning_keeps_mode_and_reports_sensor_stale_for_degraded_odometry() {
        let mut backend = ready_backend();
        backend.ingest_odometry(odometry_sample(3, StatusMode::Degraded));

        let update = step_backend(&mut backend, step_at(30_000_000));

        assert_eq!(update.mode, LocalizationMode::DeadReckoning);
        assert!(
            update
                .status
                .reasons
                .contains(&LocalizationStatusReason::SensorStale)
        );
    }

    #[test]
    fn pose_graph_queries_are_rejected_as_revision_unavailable() {
        let current = current_revision();
        let request = PoseGraphRequest {
            revision: current,
            range: PoseGraphRange::All,
            max_bytes: None,
        };

        assert_eq!(
            pose_graph_response(&request, current),
            PoseGraphResponse::RevisionUnavailable {
                latest_available: Some(current)
            }
        );
    }

    #[test]
    fn localize_pose_graph_handler_matches_response_builder() {
        let current_revision = LocalizationRevisionId {
            epoch: LOCALIZE_EPOCH,
            sequence: 7,
        };
        let view = LocalizeView { current_revision };
        let request = PoseGraphRequest {
            revision: current_revision,
            range: PoseGraphRange::All,
            max_bytes: None,
        };

        assert_eq!(
            localize_pose_graph(&view, request.clone()),
            pose_graph_response(&request, current_revision)
        );
    }

    #[test]
    fn localize_keyframe_handler_matches_response_builder() {
        let current_revision = LocalizationRevisionId {
            epoch: LOCALIZE_EPOCH,
            sequence: 7,
        };
        let view = LocalizeView { current_revision };
        let request = KeyframeRequest {
            revision: current_revision,
            keyframe_id: KeyframeId::new("kf-1"),
            max_bytes: None,
        };

        assert_eq!(
            localize_keyframe(&view, request.clone()),
            keyframe_response(&request, current_revision)
        );
    }

    #[test]
    fn localize_corrections_handler_matches_response_builder() {
        let current_revision = LocalizationRevisionId {
            epoch: LOCALIZE_EPOCH,
            sequence: 7,
        };
        let view = LocalizeView { current_revision };
        let request = CorrectionsRequest {
            from_revision: current_revision,
            to_revision: current_revision,
            max_bytes: None,
        };

        assert_eq!(
            localize_corrections(&view, request.clone()),
            corrections_response(&request, current_revision)
        );
    }

    fn ready_backend() -> DeadReckoningBackend {
        let mut backend = DeadReckoningBackend::default();
        backend.ingest_odometry(odometry_sample(1, StatusMode::Tracking));
        backend.ingest_odometry(odometry_sample(2, StatusMode::Tracking));
        backend
    }

    fn step_backend(backend: &mut DeadReckoningBackend, step: Step) -> BackendUpdate {
        match backend.step(step) {
            Ok(update) => update,
            Err(error) => panic!("dead-reckoning step failed: {error:#}"),
        }
    }

    fn step_at(time_ns: u64) -> Step {
        Step::new(Clock::new(1, time_ns / 20_000_000, time_ns, 20_000_000))
    }

    fn odometry_sample(sequence: u64, mode: StatusMode) -> Received<OdometryEstimate> {
        Received {
            at_ns: Some(sequence),
            value: OdometryEstimate {
                pose: OdometryPoseEstimate {
                    frame_id: FrameId::new("odom"),
                    child_frame_id: FrameId::new("base_footprint"),
                    translation_m: [sequence as f64, 0.0, 0.0],
                    rotation_xyzw: [0.0, 0.0, 0.0, 1.0],
                },
                velocity: OdometryVelocityEstimate {
                    frame_id: FrameId::new("base_footprint"),
                    linear_mps: [0.1, 0.0, 0.0],
                    angular_radps: [0.0, 0.0, 0.0],
                },
                covariance: Some(OdometryCovariance {
                    values: vec![0.0; 36],
                }),
                status: Status {
                    mode,
                    reasons: Vec::new(),
                },
            },
        }
    }
}
