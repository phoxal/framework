//! `perception` - detector shell for camera/depth sensing.
//!
//! This runtime subscribes to per-component camera and depth frames plus
//! `localize/state`, runs them through a detector head, and publishes
//! `perception/detections` and `perception/state`. Detections from a fresh,
//! confident localization are reported in the `map` frame; otherwise they stay in
//! the source sensor frame. A small point tracker assigns stable `track_id`s by
//! nearest same-class association within a time/distance window.
//!
//! The default detector is intentionally honest: no heavyweight model backend is
//! linked, and the deterministic placeholder emits no detections, so the runtime
//! publishes empty detection sets while still reporting camera health. Future
//! detector heads can plug in behind the small `DetectorHead` trait without
//! changing the runtime IO surface.

use anyhow::Result;
use phoxal::api::y2026_1 as api;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::Capability;
use phoxal::model::v1::Robot;
use phoxal::prelude::*;

const CAMERA_STALE_NS: u64 = 1_000_000_000;
const DEPTH_STALE_NS: u64 = 1_000_000_000;
const LOCALIZATION_STALE_NS: u64 = 1_000_000_000;
const MIN_LOCALIZATION_CONFIDENCE: f32 = 0.25;

#[derive(Clone)]
struct SensorBinding {
    component_id: String,
    capability_id: String,
    frame_id: String,
}

impl SensorBinding {
    fn from_ref(robot: &Robot, reference: CapabilityRef) -> Result<Self> {
        let frame_id = robot
            .require_link_target(&reference)
            .or_else(|_| robot.component_mount_link(&reference.component_id))?;
        Ok(Self {
            frame_id,
            component_id: reference.component_id,
            capability_id: reference.capability_id,
        })
    }

    // Perception CONSUMES sensor frames (the camera/depth drivers own/publish
    // them), so these are the client `Subscribe` side from the public builder.
    fn camera_topic(
        &self,
    ) -> phoxal::bus::Topic<phoxal::bus::Subscribe<api::component::camera::Frame>> {
        api::topic::new()
            .component(&self.component_id)
            .camera(&self.capability_id)
            .frame()
    }

    fn depth_topic(
        &self,
    ) -> phoxal::bus::Topic<phoxal::bus::Subscribe<api::component::depth::Frame>> {
        api::topic::new()
            .component(&self.component_id)
            .depth(&self.capability_id)
            .frame()
    }
}

#[derive(Clone)]
struct FrameSample<B> {
    body: B,
    produced_at_ns: u64,
}

#[derive(Default)]
struct HealthState {
    healthy: bool,
}

impl HealthState {
    fn observe_healthy(&mut self) {
        self.healthy = true;
    }

    fn degrade(&mut self) {
        self.healthy = false;
    }

    fn healthy(&self) -> bool {
        self.healthy
    }
}

struct DetectorInput<'a> {
    camera: &'a api::component::camera::Frame,
    depth: Option<&'a api::component::depth::Frame>,
    frame_id: &'a str,
    stamp_ns: u64,
    localization: Option<&'a api::localize::LocalizationState>,
}

#[derive(Clone, Debug, PartialEq)]
struct RawDetection {
    class_id: String,
    confidence: f32,
    position_m: [f64; 3],
}

trait DetectorHead {
    fn detector_name(&self) -> &'static str;
    fn detect(&mut self, input: DetectorInput<'_>) -> Vec<RawDetection>;
}

struct PlaceholderDetector;

impl DetectorHead for PlaceholderDetector {
    fn detector_name(&self) -> &'static str {
        "deterministic-placeholder"
    }

    fn detect(&mut self, input: DetectorInput<'_>) -> Vec<RawDetection> {
        let _deterministic_input_identity = (
            input.camera.width,
            input.camera.height,
            input.depth.and_then(|depth| depth.width),
            input.frame_id,
            input.stamp_ns,
            input.localization.map(|localize| localize.confidence),
        );
        Vec::new()
    }
}

#[derive(Clone, Copy)]
struct TrackerConfig {
    association_window_ns: u64,
    association_max_distance_m: f64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            association_window_ns: 500_000_000,
            association_max_distance_m: 0.5,
        }
    }
}

#[derive(Clone)]
struct Track {
    track_id: u64,
    class_id: String,
    position_m: [f64; 3],
    last_seen_ns: u64,
}

struct PointTracker {
    config: TrackerConfig,
    next_track_id: u64,
    tracks: Vec<Track>,
}

impl PointTracker {
    fn new(config: TrackerConfig) -> Self {
        Self {
            config,
            next_track_id: 1,
            tracks: Vec::new(),
        }
    }

    fn update(&mut self, detections: &mut [api::perception::Detection], observed_at_ns: u64) {
        self.prune_expired(observed_at_ns);

        let mut assigned_track_indices = Vec::new();
        for detection in detections {
            if let Some(track_index) =
                self.best_track_for(detection, observed_at_ns, &assigned_track_indices)
            {
                let track = &mut self.tracks[track_index];
                track.position_m = detection.position_m;
                track.class_id.clone_from(&detection.class_id);
                track.last_seen_ns = observed_at_ns;
                detection.track_id = Some(track.track_id);
                assigned_track_indices.push(track_index);
            } else {
                let track_id = self.next_track_id;
                self.next_track_id = self.next_track_id.saturating_add(1);
                self.tracks.push(Track {
                    track_id,
                    class_id: detection.class_id.clone(),
                    position_m: detection.position_m,
                    last_seen_ns: observed_at_ns,
                });
                detection.track_id = Some(track_id);
                assigned_track_indices.push(self.tracks.len() - 1);
            }
        }
    }

    fn prune_expired(&mut self, observed_at_ns: u64) {
        let window_ns = self.config.association_window_ns;
        self.tracks
            .retain(|track| observed_at_ns.saturating_sub(track.last_seen_ns) <= window_ns);
    }

    fn best_track_for(
        &self,
        detection: &api::perception::Detection,
        observed_at_ns: u64,
        assigned_track_indices: &[usize],
    ) -> Option<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(index, track)| {
                !assigned_track_indices.contains(index)
                    && track.class_id == detection.class_id
                    && observed_at_ns.saturating_sub(track.last_seen_ns)
                        <= self.config.association_window_ns
            })
            .filter_map(|(index, track)| {
                let distance_m = distance_m(track.position_m, detection.position_m);
                (distance_m <= self.config.association_max_distance_m)
                    .then_some((index, distance_m))
            })
            .min_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(index, _)| index)
    }
}

impl Default for PointTracker {
    fn default() -> Self {
        Self::new(TrackerConfig::default())
    }
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "perception", api = y2026_1)]
struct Perception {
    // Runtime-private state.
    camera_sources: Vec<SensorBinding>,
    depth_sources: Vec<SensorBinding>,
    latest_cameras: Vec<Option<FrameSample<api::component::camera::Frame>>>,
    latest_depths: Vec<Option<FrameSample<api::component::depth::Frame>>>,
    latest_localization: Option<FrameSample<api::localize::LocalizationState>>,
    detector: PlaceholderDetector,
    tracker: PointTracker,
    health: HealthState,
    // Handles.
    cameras: Vec<Subscriber<api::component::camera::Frame>>,
    depths: Vec<Subscriber<api::component::depth::Frame>>,
    localization: Subscriber<api::localize::LocalizationState>,
    detections: Publisher<api::perception::Detections>,
    state: Publisher<api::perception::State>,
}

#[phoxal::runtime]
impl Perception {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let camera_sources = camera_bindings(ctx.robot()?)?;
        let depth_sources = depth_bindings(ctx.robot()?)?;

        let mut cameras = Vec::with_capacity(camera_sources.len());
        for source in &camera_sources {
            cameras.push(ctx.subscribe(source.camera_topic()).subscriber().await?);
        }

        let mut depths = Vec::with_capacity(depth_sources.len());
        for source in &depth_sources {
            depths.push(ctx.subscribe(source.depth_topic()).subscriber().await?);
        }

        Ok(Self {
            latest_cameras: vec![None; camera_sources.len()],
            latest_depths: vec![None; depth_sources.len()],
            latest_localization: None,
            detector: PlaceholderDetector,
            tracker: PointTracker::default(),
            health: HealthState::default(),
            cameras,
            depths,
            localization: ctx
                .subscribe(api::topic::new().localize().state())
                .subscriber()
                .await?,
            // Perception OWNS the `perception` node (detections + state telemetry)
            // -> owner (`internal`) builder; sensor frames and `localize/state` are
            // CONSUMED via the public builder.
            detections: ctx
                .publisher(api::topic::internal::new().perception().detections())
                .await?,
            state: ctx
                .publisher(api::topic::internal::new().perception().state())
                .await?,
            camera_sources,
            depth_sources,
        })
    }

    #[step(hz = 10)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        self.drain_inputs();

        let now_ns = step.time().time_ns();
        let detections = self.detect(now_ns);
        let healthy = !detections.is_empty()
            || latest_fresh_camera_index(&self.latest_cameras, now_ns, CAMERA_STALE_NS).is_some();
        if healthy {
            self.health.observe_healthy();
        } else {
            self.health.degrade();
        }

        self.detections
            .publish_at(
                step.time(),
                api::perception::Detections {
                    detections,
                    stamp_ns: Some(now_ns),
                },
            )
            .await?;
        self.state
            .publish_at(
                step.time(),
                api::perception::State {
                    healthy: self.health.healthy(),
                    detector: self.detector.detector_name().to_string(),
                },
            )
            .await?;
        Ok(())
    }
}

impl Perception {
    fn drain_inputs(&mut self) {
        for (subscriber, slot) in self.cameras.iter().zip(&mut self.latest_cameras) {
            while let Some(received) = subscriber.try_recv() {
                *slot = Some(FrameSample {
                    body: received.body,
                    produced_at_ns: received.metadata.produced_at_ns,
                });
            }
        }
        for (subscriber, slot) in self.depths.iter().zip(&mut self.latest_depths) {
            while let Some(received) = subscriber.try_recv() {
                *slot = Some(FrameSample {
                    body: received.body,
                    produced_at_ns: received.metadata.produced_at_ns,
                });
            }
        }
        while let Some(received) = self.localization.try_recv() {
            self.latest_localization = Some(FrameSample {
                body: received.body,
                produced_at_ns: received.metadata.produced_at_ns,
            });
        }
    }

    fn detect(&mut self, now_ns: u64) -> Vec<api::perception::Detection> {
        let Some(camera_index) =
            latest_fresh_camera_index(&self.latest_cameras, now_ns, CAMERA_STALE_NS)
        else {
            return Vec::new();
        };
        let camera = self.latest_cameras[camera_index]
            .as_ref()
            .expect("fresh index must point at a camera sample")
            .clone();
        let source = self.camera_sources[camera_index].clone();
        let depth = latest_matching_depth(
            &self.depth_sources,
            &self.latest_depths,
            &source.component_id,
            now_ns,
        );
        let localization = fresh_localization(&self.latest_localization, now_ns).cloned();
        let stamp_ns = camera.body.measured_at_ns.unwrap_or(camera.produced_at_ns);

        let raw = detect_with(
            &mut self.detector,
            DetectorInput {
                camera: &camera.body,
                depth: depth.as_ref().map(|sample| &sample.body),
                frame_id: &source.frame_id,
                stamp_ns,
                localization: localization.as_ref(),
            },
        );
        let mut detections = raw
            .into_iter()
            .map(|raw| detection_from_raw(raw, &source.frame_id, localization.as_ref()))
            .collect::<Vec<_>>();
        self.tracker.update(&mut detections, stamp_ns);
        detections
    }
}

fn detect_with(detector: &mut impl DetectorHead, input: DetectorInput<'_>) -> Vec<RawDetection> {
    detector.detect(input)
}

fn camera_bindings(robot: &Robot) -> Result<Vec<SensorBinding>> {
    robot
        .camera_capabilities()
        .into_iter()
        .map(|reference| SensorBinding::from_ref(robot, reference))
        .collect()
}

fn depth_bindings(robot: &Robot) -> Result<Vec<SensorBinding>> {
    let mut references = Vec::new();
    for component_id in robot.manifest.components.instances.keys() {
        let component = robot.component_for_instance(component_id)?;
        for (capability_id, capability) in &component.capabilities {
            if matches!(capability, Capability::Depth(_)) {
                references.push(CapabilityRef::new(component_id, capability_id));
            }
        }
    }
    references.sort();
    references
        .into_iter()
        .map(|reference| SensorBinding::from_ref(robot, reference))
        .collect()
}

fn latest_fresh_camera_index<B>(
    samples: &[Option<FrameSample<B>>],
    now_ns: u64,
    stale_ns: u64,
) -> Option<usize> {
    samples
        .iter()
        .enumerate()
        .filter_map(|(index, sample)| sample.as_ref().map(|sample| (index, sample)))
        .filter(|(_, sample)| now_ns.saturating_sub(sample.produced_at_ns) <= stale_ns)
        .max_by_key(|(_, sample)| sample.produced_at_ns)
        .map(|(index, _)| index)
}

fn latest_matching_depth(
    depth_sources: &[SensorBinding],
    samples: &[Option<FrameSample<api::component::depth::Frame>>],
    component_id: &str,
    now_ns: u64,
) -> Option<FrameSample<api::component::depth::Frame>> {
    depth_sources
        .iter()
        .zip(samples)
        .filter(|(source, _)| source.component_id == component_id)
        .filter_map(|(_, sample)| sample.as_ref())
        .filter(|sample| now_ns.saturating_sub(sample.produced_at_ns) <= DEPTH_STALE_NS)
        .max_by_key(|sample| sample.produced_at_ns)
        .cloned()
}

fn fresh_localization(
    sample: &Option<FrameSample<api::localize::LocalizationState>>,
    now_ns: u64,
) -> Option<&api::localize::LocalizationState> {
    let sample = sample.as_ref()?;
    if now_ns.saturating_sub(sample.produced_at_ns) > LOCALIZATION_STALE_NS {
        return None;
    }
    (sample.body.confidence >= MIN_LOCALIZATION_CONFIDENCE).then_some(&sample.body)
}

fn detection_from_raw(
    raw: RawDetection,
    source_frame_id: &str,
    localization: Option<&api::localize::LocalizationState>,
) -> api::perception::Detection {
    let (position_m, frame_id) = match localization {
        Some(localization) => (
            local_to_map_position(raw.position_m, localization),
            "map".to_string(),
        ),
        None => (raw.position_m, source_frame_id.to_string()),
    };
    api::perception::Detection {
        class_id: raw.class_id,
        confidence: raw.confidence,
        position_m,
        frame_id,
        track_id: None,
    }
}

fn local_to_map_position(
    position_m: [f64; 3],
    localization: &api::localize::LocalizationState,
) -> [f64; 3] {
    let yaw_cos = localization.yaw_rad.cos();
    let yaw_sin = localization.yaw_rad.sin();
    [
        localization.x_m + yaw_cos * position_m[0] - yaw_sin * position_m[1],
        localization.y_m + yaw_sin * position_m[0] + yaw_cos * position_m[1],
        position_m[2],
    ]
}

fn distance_m(left: [f64; 3], right: [f64; 3]) -> f64 {
    let dx = left[0] - right[0];
    let dy = left[1] - right[1];
    let dz = left[2] - right[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Perception>()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use phoxal::api::ContractBody;

    use super::*;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixture/robot/rgbd-imu-diff-drive")
    }

    fn raw(position_m: [f64; 3]) -> RawDetection {
        RawDetection {
            class_id: "crate".to_string(),
            confidence: 0.9,
            position_m,
        }
    }

    #[test]
    fn placeholder_detector_emits_no_detections() {
        let mut detector = PlaceholderDetector;
        let camera = api::component::camera::Frame {
            width: 2,
            height: 2,
            encoding: api::component::camera::Encoding::Rgb8,
            intrinsics: None,
            distortion: None,
            exposure: None,
            measured_at_ns: Some(123),
            calibration: None,
            data: vec![0; 12],
        };

        let detections = detector.detect(DetectorInput {
            camera: &camera,
            depth: None,
            frame_id: "front_camera__rgb_link",
            stamp_ns: 123,
            localization: None,
        });

        assert!(detections.is_empty());
        assert_eq!(detector.detector_name(), "deterministic-placeholder");
    }

    #[test]
    fn point_tracker_reuses_nearby_track_and_separates_distant_detection() {
        let mut tracker = PointTracker::new(TrackerConfig {
            association_window_ns: 1_000,
            association_max_distance_m: 0.5,
        });
        let mut first = vec![detection_from_raw(raw([1.0, 0.0, 0.0]), "camera", None)];
        tracker.update(&mut first, 100);
        let first_id = first[0].track_id;

        let mut nearby = vec![detection_from_raw(raw([1.2, 0.0, 0.0]), "camera", None)];
        tracker.update(&mut nearby, 200);
        assert_eq!(nearby[0].track_id, first_id);

        let mut distant = vec![detection_from_raw(raw([5.0, 0.0, 0.0]), "camera", None)];
        tracker.update(&mut distant, 300);
        assert_ne!(distant[0].track_id, first_id);
    }

    #[test]
    fn detection_uses_map_frame_when_localization_is_fresh() {
        let localization = api::localize::LocalizationState {
            x_m: 2.0,
            y_m: 3.0,
            yaw_rad: std::f64::consts::FRAC_PI_2,
            confidence: 1.0,
        };

        let detection = detection_from_raw(raw([1.0, 0.0, 0.5]), "camera", Some(&localization));

        assert_eq!(detection.frame_id, "map");
        assert!((detection.position_m[0] - 2.0).abs() < 1e-9);
        assert!((detection.position_m[1] - 4.0).abs() < 1e-9);
        assert_eq!(detection.position_m[2], 0.5);
    }

    #[test]
    fn sensor_bindings_from_robot_enumerate_camera_and_depth_topics() {
        let robot = Robot::read_from_dir(fixture()).unwrap();
        let cameras = camera_bindings(&robot).unwrap();
        let depths = depth_bindings(&robot).unwrap();

        assert_eq!(cameras.len(), 3);
        assert_eq!(depths.len(), 1);
        assert!(
            cameras
                .iter()
                .any(|camera| camera.camera_topic().key()
                    == "component/front_camera/camera/rgb/frame")
        );
        assert_eq!(
            depths[0].depth_topic().key(),
            "component/front_camera/depth/depth/frame"
        );
    }

    #[test]
    fn emit_apis_reports_perception_contracts() {
        let json = phoxal::runtime::emit_apis_json::<Perception>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "perception");
        assert_eq!(value["api_version"], "y2026_1");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert_contract::<api::component::camera::Frame>(contracts, "subscribe");
        assert_contract::<api::component::depth::Frame>(contracts, "subscribe");
        assert_contract::<api::localize::LocalizationState>(contracts, "subscribe");
        assert_contract::<api::perception::Detections>(contracts, "publish");
        assert_contract::<api::perception::State>(contracts, "publish");
    }

    fn assert_contract<B>(contracts: &[serde_json::Value], direction: &str)
    where
        B: ContractBody,
    {
        assert!(contracts.iter().any(|contract| {
            contract["family"] == B::FAMILY
                && contract["topic"] == B::TOPIC
                && contract["direction"] == direction
        }));
    }
}
