//! `perception` - detector shell for camera/depth sensing.
//!
//! This participant subscribes to per-component camera and depth frames plus
//! `localize/state`, runs them through a detector head, and publishes
//! `perception/detections` and `perception/state`. Detections from a fresh,
//! confident localization are reported in the `map` frame; otherwise they stay in
//! the source sensor frame. A small point tracker assigns stable `track_id`s by
//! nearest same-class association within a time/distance window.
//!
//! The default detector is intentionally honest: no heavyweight model backend is
//! linked, and the deterministic placeholder emits no detections, so the participant
//! publishes empty detection sets while still reporting camera health. Future
//! detector heads can plug in behind the small `DetectorHead` trait without
//! changing the participant IO surface.

mod detector;
mod frames;
mod sensors;
mod tracker;

use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::v1 as api;

use crate::detector::{DetectorHead, DetectorInput, PlaceholderDetector, detect_with};
use crate::frames::{
    FrameSample, HealthState, detection_from_raw, fresh_localization, latest_fresh_camera_index,
    latest_matching_depth,
};
use crate::sensors::{SensorBinding, camera_bindings, depth_bindings};
use crate::tracker::PointTracker;

const CAMERA_STALE_NS: u64 = 1_000_000_000;
const DEPTH_STALE_NS: u64 = 1_000_000_000;
const LOCALIZATION_STALE_NS: u64 = 1_000_000_000;
const MIN_LOCALIZATION_CONFIDENCE: f32 = 0.25;

#[derive(phoxal::Api)]
struct Api {
    cameras: Vec<Subscriber<api::component::camera::Frame>>,
    depths: Vec<Subscriber<api::component::depth::Frame>>,
    localization: Subscriber<api::localize::LocalizationState>,
    detections: Publisher<api::perception::Detections>,
    state: Publisher<api::perception::State>,
}

#[phoxal::service(id = "perception", config = ())]
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
}

#[phoxal::behavior]
impl Perception {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        let camera_sources = camera_bindings(ctx.robot()?)?;
        let depth_sources = depth_bindings(ctx.robot()?)?;

        let mut cameras = Vec::with_capacity(camera_sources.len());
        for source in &camera_sources {
            cameras.push(ctx.subscriber(source.camera_topic(), 32).await?);
        }

        let mut depths = Vec::with_capacity(depth_sources.len());
        for source in &depth_sources {
            depths.push(ctx.subscriber(source.depth_topic(), 32).await?);
        }

        let localization = ctx
            .subscriber(api::topic::new().localize().state(), 32)
            .await?;
        // Perception OWNS the `perception` node (detections + state telemetry)
        // -> owner (`internal`) builder; sensor frames and `localize/state` are
        // CONSUMED via the public builder.
        let detections = ctx
            .publisher(api::topic::internal::new(cap).perception().detections())
            .await?;
        let state = ctx
            .publisher(api::topic::internal::new(cap).perception().state())
            .await?;

        Ok((
            Self {
                latest_cameras: vec![None; camera_sources.len()],
                latest_depths: vec![None; depth_sources.len()],
                latest_localization: None,
                detector: PlaceholderDetector,
                tracker: PointTracker::default(),
                health: HealthState::default(),
                camera_sources,
                depth_sources,
            },
            Self::Api {
                cameras,
                depths,
                localization,
                detections,
                state,
            },
        ))
    }

    #[step(hz = 10)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        self.drain_inputs(api);

        let now = step.time();
        if self.camera_sources.is_empty()
            || latest_fresh_camera_index(&self.latest_cameras, now, CAMERA_STALE_NS).is_none()
        {
            return Ok(());
        }
        let detections = self.detect(now);
        let healthy = !detections.is_empty()
            || latest_fresh_camera_index(&self.latest_cameras, now, CAMERA_STALE_NS).is_some();
        if healthy {
            self.health.observe_healthy();
        } else {
            self.health.degrade();
        }

        api.detections
            .publish_at(
                step.time(),
                api::perception::Detections {
                    detections,
                    stamp_ns: Some(now.time_ns()),
                },
            )
            .await?;
        api.state
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
    fn drain_inputs(&mut self, api: &mut Api) {
        for (subscriber, slot) in api.cameras.iter().zip(&mut self.latest_cameras) {
            while let Some(received) = subscriber.try_recv() {
                *slot = Some(FrameSample {
                    body: received.body,
                    at: LogicalTime::new(received.metadata.epoch, received.metadata.produced_at_ns),
                });
            }
        }
        for (subscriber, slot) in api.depths.iter().zip(&mut self.latest_depths) {
            while let Some(received) = subscriber.try_recv() {
                *slot = Some(FrameSample {
                    body: received.body,
                    at: LogicalTime::new(received.metadata.epoch, received.metadata.produced_at_ns),
                });
            }
        }
        while let Some(received) = api.localization.try_recv() {
            self.latest_localization = Some(FrameSample {
                body: received.body,
                at: LogicalTime::new(received.metadata.epoch, received.metadata.produced_at_ns),
            });
        }
    }

    fn detect(&mut self, now: LogicalTime) -> Vec<api::perception::Detection> {
        let Some(camera_index) =
            latest_fresh_camera_index(&self.latest_cameras, now, CAMERA_STALE_NS)
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
            now,
            DEPTH_STALE_NS,
        );
        let localization = fresh_localization(
            &self.latest_localization,
            now,
            LOCALIZATION_STALE_NS,
            MIN_LOCALIZATION_CONFIDENCE,
        )
        .cloned();
        let stamp_ns = camera.body.measured_at_ns.unwrap_or(camera.at.time_ns());

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

fn main() -> phoxal::Result<()> {
    phoxal::run::<Perception>()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use phoxal::model::v0::Robot;
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};
    use phoxal_api::ContractBody;

    use super::*;
    use crate::detector::RawDetection;
    use crate::tracker::TrackerConfig;

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
            cameras.iter().any(|camera| camera.camera_topic().key()
                == "v1/component/front_camera/camera/rgb/frame")
        );
        assert_eq!(
            depths[0].depth_topic().key(),
            "v1/component/front_camera/depth/depth/frame"
        );
    }

    #[test]
    fn api_reports_perception_contracts() {
        assert_eq!(<Perception as Participant>::ID, "perception");

        let contracts = <<Perception as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert_contract::<api::component::camera::Frame>(contracts, ContractRole::Subscribe);
        assert_contract::<api::component::depth::Frame>(contracts, ContractRole::Subscribe);
        assert_contract::<api::localize::LocalizationState>(contracts, ContractRole::Subscribe);
        assert_contract::<api::perception::Detections>(contracts, ContractRole::Publish);
        assert_contract::<api::perception::State>(contracts, ContractRole::Publish);
    }

    fn assert_contract<B>(contracts: &[phoxal::participant::ApiContractUse], role: ContractRole)
    where
        B: ContractBody,
    {
        assert!(
            contracts
                .iter()
                .any(|c| c.topic == B::TOPIC && c.role == role),
            "expected a {role:?} contract for {} in {contracts:?}",
            B::TOPIC
        );
    }
}
