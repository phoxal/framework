use std::time::Duration;

use crate::core::{HealthState, Tracker, TrackerConfig};
use anyhow::{Result, bail};
use phoxal::api::component::v1::capability::{camera, depth};
use phoxal::api::frame::v1::{FrameId, Tree, tree};
use phoxal::api::localize::v1::LocalizationState;
use phoxal::api::map::v1::{MapRevision, revision};
use phoxal::api::perception::v1::{
    BoundingBox, Detection, Detections, PerceptionDegradedReason, PerceptionState,
    PerceptionStoppedReason, RevisionLinkage, detections, state,
};
use phoxal::bus::pubsub::Stamped;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::Capability;
use phoxal::model::robot::v1::Role;
use phoxal::model::structure::Structure;
use phoxal::runtime::clock::Step;
use phoxal::runtime::runtime::{InputPolicy, Io, Publisher, Runtime, RuntimeInputs};
use phoxal::runtime::staged::Robot;
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};
use tracing::warn;

const CLOCK_PERIOD: Duration = Duration::from_millis(50);
const CADENCE_HZ: f32 = 5.0;
const INFERENCE_PERIOD_NS: u64 = 200_000_000;
const INFERENCE_BUDGET_NS: u64 = 20_000_000;
const PLACEHOLDER_DETECTOR_ID: &str = "placeholder";
const PLACEHOLDER_BACKEND: &str = "deterministic-placeholder";
const PLACEHOLDER_MODEL_ID: &str = "placeholder-object-detector";
const PLACEHOLDER_WEIGHTS_VERSION: &str = "none";

#[derive(Debug, Clone)]
pub(crate) struct PerceptionSource {
    camera: CapabilityRef,
    depth: CapabilityRef,
    camera_topic: String,
    depth_topic: String,
    source_frame_id: FrameId,
}

#[derive(Clone)]
pub(crate) struct Config {
    clock_period: Duration,
    source: Option<PerceptionSource>,
    detector_id: String,
    backend: String,
    model_id: String,
    weights_version: String,
    cadence_hz: f32,
}

impl Config {
    pub(crate) fn from_robot(robot: &Robot, structure: &Structure) -> Result<Self> {
        Ok(Self {
            clock_period: CLOCK_PERIOD,
            source: PerceptionSource::from_robot(robot, structure)?,
            detector_id: PLACEHOLDER_DETECTOR_ID.to_string(),
            backend: PLACEHOLDER_BACKEND.to_string(),
            model_id: PLACEHOLDER_MODEL_ID.to_string(),
            weights_version: PLACEHOLDER_WEIGHTS_VERSION.to_string(),
            cadence_hz: CADENCE_HZ,
        })
    }

    pub(crate) const fn clock_period(&self) -> Duration {
        self.clock_period
    }
}

pub(crate) enum Input {
    Camera(Stamped<camera::Frame>),
    Depth(Stamped<depth::Depth>),
    LocalizationState(Stamped<LocalizationState>),
    FrameTree(Stamped<Tree>),
    MapRevision(Stamped<MapRevision>),
}

pub(crate) struct PerceptionRuntime {
    source: Option<PerceptionSource>,
    detector: PlaceholderDetector,
    tracker: Tracker,
    health: HealthState,
    latest_camera: Option<Stamped<camera::Frame>>,
    latest_depth: Option<Stamped<depth::Depth>>,
    latest_localize: Option<Stamped<LocalizationState>>,
    latest_frame_tree: Option<Stamped<Tree>>,
    latest_map_revision: Option<Stamped<MapRevision>>,
    next_inference_ns: u64,
    dropped_frames: u64,
    detector_id: String,
    backend: String,
    model_id: String,
    weights_version: String,
    cadence_hz: f32,
    detections_publisher: Publisher<Stamped<Detections>>,
    state_publisher: Publisher<Stamped<PerceptionState>>,
}

#[async_trait::async_trait]
impl Runtime for PerceptionRuntime {
    const RUNTIME_ID: &'static str = "perception";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Config::from_robot(&common.robot()?, &common.structure()?)
    }

    fn clock_period(config: &Self::Config) -> Duration {
        config.clock_period()
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        if let Some(source) = &config.source {
            let _ = (&source.camera, &source.depth);
            io.subscribe_with::<Stamped<camera::Frame>, _>(
                &source.camera_topic,
                InputPolicy::latest(),
                Input::Camera,
            )
            .await?;
            io.subscribe_with::<Stamped<depth::Depth>, _>(
                &source.depth_topic,
                InputPolicy::latest(),
                Input::Depth,
            )
            .await?;
        } else {
            warn!("perception runtime started without perception-role camera/depth source");
        }
        io.subscribe::<Stamped<LocalizationState>, _>(
            phoxal::api::localize::v1::state::TOPIC,
            Input::LocalizationState,
        )
        .await?;
        io.subscribe::<Stamped<Tree>, _>(tree::TOPIC, Input::FrameTree)
            .await?;
        io.subscribe::<Stamped<MapRevision>, _>(revision::TOPIC, Input::MapRevision)
            .await?;

        Ok(Self {
            source: config.source,
            detector: PlaceholderDetector,
            tracker: Tracker::new(TrackerConfig::default()),
            health: HealthState::new(),
            latest_camera: None,
            latest_depth: None,
            latest_localize: None,
            latest_frame_tree: None,
            latest_map_revision: None,
            next_inference_ns: 0,
            dropped_frames: 0,
            detector_id: config.detector_id,
            backend: config.backend,
            model_id: config.model_id,
            weights_version: config.weights_version,
            cadence_hz: config.cadence_hz,
            detections_publisher: io
                .publisher::<Stamped<Detections>>(detections::TOPIC)
                .await?,
            state_publisher: io
                .publisher::<Stamped<PerceptionState>>(state::TOPIC)
                .await?,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        let now_ns = step.tick.time_ns();
        let mut camera_inputs = 0_u64;
        for input in inputs {
            match input {
                Input::Camera(sample) => {
                    camera_inputs += 1;
                    self.latest_camera = Some(sample);
                }
                Input::Depth(sample) => self.latest_depth = Some(sample),
                Input::LocalizationState(sample) => self.latest_localize = Some(sample),
                Input::FrameTree(sample) => self.latest_frame_tree = Some(sample),
                Input::MapRevision(sample) => self.latest_map_revision = Some(sample),
            }
        }

        if now_ns < self.next_inference_ns {
            self.dropped_frames = self.dropped_frames.saturating_add(camera_inputs);
            self.publish_state(now_ns, 0.0).await?;
            return Ok(());
        }
        self.next_inference_ns = now_ns.saturating_add(INFERENCE_PERIOD_NS);

        let Some(source) = self.source.clone() else {
            self.health.stop(PerceptionStoppedReason::SourceUnavailable);
            self.publish_state(now_ns, 0.0).await?;
            return Ok(());
        };
        let Some(camera) = self.latest_camera.clone() else {
            self.health.degrade(PerceptionDegradedReason::SourceStale);
            self.publish_state(now_ns, 0.0).await?;
            return Ok(());
        };
        let Some(revision_linkage) = self.current_revision_linkage() else {
            self.health
                .degrade(PerceptionDegradedReason::LocalizationDegraded);
            self.publish_state(now_ns, 0.0).await?;
            return Ok(());
        };
        if !self.frame_tree_contains(&source.source_frame_id) {
            self.health.degrade(PerceptionDegradedReason::SourceStale);
            self.publish_state(now_ns, 0.0).await?;
            return Ok(());
        }

        let raw_detections = self.detector.infer(DetectorInput {
            camera: &camera.data,
            depth: self.latest_depth.as_ref().map(|sample| &sample.data),
            source_frame_id: &source.source_frame_id,
            timestamp_ns: camera.timestamp_ns,
        });
        let detections = raw_detections
            .into_iter()
            .map(|raw| raw.to_detection(source.source_frame_id.clone()))
            .collect::<Vec<_>>();
        let update = self.tracker.update(detections, revision_linkage, now_ns);

        self.detections_publisher
            .put(&Stamped::new(
                now_ns,
                Detections {
                    detections: update.detections,
                    localize_revision: revision_linkage.localize_revision,
                    map_revision: revision_linkage.map_revision,
                    detector_id: self.detector_id.clone(),
                },
            ))
            .await?;
        self.health.observe_healthy();
        self.publish_state(now_ns, INFERENCE_BUDGET_NS as f32)
            .await?;

        Ok(())
    }
}

impl PerceptionRuntime {
    fn current_revision_linkage(&self) -> Option<RevisionLinkage> {
        let localize_revision = self.latest_localize.as_ref()?.data.revision?;
        let map_revision = self.latest_map_revision.as_ref()?;
        (map_revision.data.built_from_localize_revision == localize_revision).then_some(
            RevisionLinkage {
                localize_revision,
                map_revision: map_revision.data.map_revision_id,
            },
        )
    }

    fn frame_tree_contains(&self, source_frame_id: &FrameId) -> bool {
        self.latest_frame_tree.as_ref().is_some_and(|tree| {
            tree.data
                .frames
                .iter()
                .any(|link| &link.frame_id == source_frame_id)
        })
    }

    async fn publish_state(&self, timestamp_ns: u64, headroom_ns: f32) -> Result<()> {
        self.state_publisher
            .put(&Stamped::new(
                timestamp_ns,
                PerceptionState {
                    health: self.health.health(),
                    backend: self.backend.clone(),
                    model_id: self.model_id.clone(),
                    weights_version: self.weights_version.clone(),
                    inference_budget_headroom: headroom_ns / 1_000_000.0,
                    cadence_hz: self.cadence_hz,
                    dropped_frames: self.dropped_frames,
                },
            ))
            .await
    }
}

impl PerceptionSource {
    fn from_robot(robot: &Robot, structure: &Structure) -> Result<Option<Self>> {
        let camera = first_role_capability(robot, Role::Perception, |capability| {
            matches!(capability, Capability::Camera(_))
        });
        let depth = first_role_capability(robot, Role::Perception, |capability| {
            matches!(capability, Capability::Depth(_))
        });
        match (camera, depth) {
            (Some(camera), Some(depth)) => {
                let source_frame_id = FrameId::new(robot.require_link_target(&camera, structure)?);
                Ok(Some(Self {
                    camera_topic: phoxal::api::component::v1::capability::default_profile_path(
                        &camera.component_id,
                        &camera.capability_id,
                    ),
                    depth_topic: phoxal::api::component::v1::capability::default_profile_path(
                        &depth.component_id,
                        &depth.capability_id,
                    ),
                    camera,
                    depth,
                    source_frame_id,
                }))
            }
            (None, None) => Ok(None),
            _ => bail!(
                "perception runtime requires both a camera and depth capability tagged with role 'perception'"
            ),
        }
    }
}

pub(crate) trait DetectorHead {
    fn infer(&self, input: DetectorInput<'_>) -> Vec<RawDetection>;
}

pub(crate) struct DetectorInput<'a> {
    camera: &'a camera::Frame,
    depth: Option<&'a depth::Depth>,
    source_frame_id: &'a FrameId,
    timestamp_ns: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawDetection {
    class_label: String,
    class_id: u32,
    confidence: f32,
    bbox: BoundingBox,
    anchor_3d_m: Option<[f64; 3]>,
}

impl RawDetection {
    fn to_detection(&self, source_frame_id: FrameId) -> Detection {
        Detection {
            class_label: self.class_label.clone(),
            class_id: self.class_id,
            confidence: self.confidence,
            bbox: self.bbox,
            anchor_3d_m: self.anchor_3d_m,
            source_frame_id,
            tracker_id: None,
        }
    }
}

pub(crate) struct PlaceholderDetector;

impl DetectorHead for PlaceholderDetector {
    fn infer(&self, input: DetectorInput<'_>) -> Vec<RawDetection> {
        let _ = (input.source_frame_id, input.timestamp_ns);
        if input.camera.width() == 0 || input.camera.height() == 0 || input.camera.data().is_empty()
        {
            return Vec::new();
        }
        if input.camera.data()[0] % 2 == 1 {
            return Vec::new();
        }
        let width = input.camera.width() as f32;
        let height = input.camera.height() as f32;
        let depth_m = input
            .depth
            .and_then(|depth| depth.samples_mm().first().copied())
            .map(|sample_mm| f64::from(sample_mm) / f64::from(depth::MILLIMETERS_PER_METER));
        vec![RawDetection {
            class_label: "placeholder_object".to_string(),
            class_id: 0,
            confidence: 0.5,
            bbox: BoundingBox {
                x: width * 0.25,
                y: height * 0.25,
                width: width * 0.5,
                height: height * 0.5,
            },
            anchor_3d_m: depth_m.map(|z_m| [z_m, 0.0, 0.0]),
        }]
    }
}

fn first_role_capability(
    robot: &Robot,
    role: Role,
    predicate: impl Fn(&Capability) -> bool,
) -> Option<CapabilityRef> {
    robot
        .model
        .components
        .iter()
        .flat_map(|(component_id, component)| {
            component
                .roles
                .iter()
                .filter(move |(_, roles)| roles.contains(&role))
                .map(move |(capability_id, _)| CapabilityRef::new(component_id, capability_id))
        })
        .find(|capability_ref| robot.capability(capability_ref).is_ok_and(&predicate))
}
