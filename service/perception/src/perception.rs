//! `perception` - detector shell for camera/depth sensing.
//!
//! This participant subscribes to per-component camera and depth frames plus
//! `localize/state`, runs them through a detector head, and publishes
//! `perception/detections` and `perception/state`. Detections from a fresh,
//! confident localization are reported in the `map` frame; otherwise they stay in
//! the source sensor frame. A small point tracker assigns stable `track_id`s by
//! nearest same-class association within a time/distance window.
//!
//! The default detector is honest: no heavyweight model backend is linked, so
//! the participant publishes empty detection sets while still reporting camera
//! health.

use phoxal::api;
use phoxal::model::identity::ComponentInstanceId;
use phoxal::prelude::*;

use crate::detector::{DetectorHead, DetectorInput, PlaceholderDetector};
use crate::sensors::SensorBinding;
use crate::tracker::PointTracker;

const CAMERA_STALE: std::time::Duration = std::time::Duration::from_nanos(1_000_000_000);
const DEPTH_STALE: std::time::Duration = std::time::Duration::from_nanos(1_000_000_000);
const LOCALIZATION_STALE: std::time::Duration = std::time::Duration::from_nanos(1_000_000_000);
const MIN_LOCALIZATION_CONFIDENCE: f32 = 0.25;

pub(crate) struct Api {
    cameras: Vec<Subscriber<api::component::camera::Frame>>,
    depths: Vec<Subscriber<api::component::depth::Frame>>,
    localization: Subscriber<api::localize::LocalizationState>,
    detections: StatePublisher<api::perception::Detections>,
    state: StatePublisher<api::perception::State>,
}

pub(crate) struct PerceptionState {
    // Runtime-private state. `camera_sources` and `latest_cameras` are index
    // coupled and built together, as are `depth_sources` and `latest_depths`.
    camera_sources: Vec<SensorBinding>,
    depth_sources: Vec<SensorBinding>,
    latest_cameras: Vec<Option<Timed<api::component::camera::Frame>>>,
    latest_depths: Vec<Option<Timed<api::component::depth::Frame>>>,
    latest_localization: Option<Timed<api::localize::LocalizationState>>,
    detector: PlaceholderDetector,
    tracker: PointTracker,
    healthy: bool,
}

#[phoxal::service(state = PerceptionState, api = Api)]
pub(crate) struct Perception;

impl Participant for Perception {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let camera_sources = SensorBinding::cameras(ctx.robot()?)?;
        let depth_sources = SensorBinding::depths(ctx.robot()?)?;

        let mut cameras = Vec::with_capacity(camera_sources.len());
        for source in &camera_sources {
            cameras.push(ctx.subscriber(source.camera_topic(), 32).await?);
        }

        let mut depths = Vec::with_capacity(depth_sources.len());
        for source in &depth_sources {
            depths.push(ctx.subscriber(source.depth_topic(), 32).await?);
        }

        let localization = ctx
            .subscriber(api::topic::client().localize().state(), 32)
            .await?;
        // Perception OWNS the `perception` node (detections + state telemetry)
        // -> owner builder; sensor frames and `localize/state` are
        // CONSUMED via the public builder.
        let detections = ctx
            .state_publisher(api::topic::owner().perception().detections())
            .await?;
        let state = ctx
            .state_publisher(api::topic::owner().perception().state())
            .await?;

        Ok((
            PerceptionState {
                latest_cameras: vec![None; camera_sources.len()],
                latest_depths: vec![None; depth_sources.len()],
                latest_localization: None,
                detector: PlaceholderDetector,
                tracker: PointTracker::default(),
                healthy: false,
                camera_sources,
                depth_sources,
            },
            Api {
                cameras,
                depths,
                localization,
                detections,
                state,
            },
        ))
    }

    fn reset(&self, _ctx: ResetContext, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        state.latest_cameras.fill(None);
        state.latest_depths.fill(None);
        state.latest_localization = None;
        state.tracker = PointTracker::default();
        state.healthy = false;
        Ok(())
    }

    #[phoxal::step(hz = 10)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        state.drain_inputs(api);

        let now = step.now();
        // Nothing is published until a camera frame within the staleness window
        // exists: an empty detection set from a silent camera is indistinguishable
        // from an empty detection set from a working one.
        if state.freshest_camera(now).is_none() {
            return Ok(());
        }
        let detections = state.detect(now);
        state.healthy = !detections.is_empty() || state.freshest_camera(now).is_some();

        api.detections.publish(
            &step.token,
            api::perception::Detections {
                detections,
                stamp: Some(now),
            },
        )?;
        api.state.publish(
            &step.token,
            api::perception::State {
                healthy: state.healthy,
                detector: state.detector.detector_name().to_string(),
            },
        )?;
        Ok(())
    }
}

impl PerceptionState {
    fn drain_inputs(&mut self, api: &Api) {
        drain_latest_per_source(&api.cameras, &mut self.latest_cameras);
        drain_latest_per_source(&api.depths, &mut self.latest_depths);
        drain_latest(&api.localization, &mut self.latest_localization);
    }

    /// The newest camera frame still inside the staleness window, together with
    /// the binding it arrived on.
    ///
    /// This is the gate the step runs on, and it hands back the sample it
    /// selected rather than an index into a slice the caller would have to
    /// unwrap again.
    fn freshest_camera(
        &self,
        now: RobotInstant,
    ) -> Option<(&SensorBinding, &Timed<api::component::camera::Frame>)> {
        self.camera_sources
            .iter()
            .zip(&self.latest_cameras)
            .filter_map(|(source, sample)| sample.as_ref().map(|sample| (source, sample)))
            .filter(|(_, sample)| sample.fresh_within(now, CAMERA_STALE))
            .max_by_key(|(_, sample)| sample.at.ticks())
    }

    /// The newest fresh depth frame from the same component as `component_id`.
    ///
    /// Pairing is by component, not by capability: a camera and the depth
    /// stream registered to it are two capabilities of one sensor head.
    fn latest_matching_depth(
        &self,
        component_id: &ComponentInstanceId,
        now: RobotInstant,
    ) -> Option<&Timed<api::component::depth::Frame>> {
        self.depth_sources
            .iter()
            .zip(&self.latest_depths)
            .filter(|(source, _)| source.component_id() == component_id)
            .filter_map(|(_, sample)| sample.as_ref())
            .filter(|sample| sample.fresh_within(now, DEPTH_STALE))
            .max_by_key(|sample| sample.at.ticks())
    }

    /// The latest localization, if it is fresh and confident enough to move a
    /// detection out of the sensor frame and into the map frame.
    fn fresh_localization(&self, now: RobotInstant) -> Option<&api::localize::LocalizationState> {
        let sample = self.latest_localization.as_ref()?;
        if !sample.fresh_within(now, LOCALIZATION_STALE) {
            return None;
        }
        (sample.body.confidence >= MIN_LOCALIZATION_CONFIDENCE).then_some(&sample.body)
    }

    fn detect(&mut self, now: RobotInstant) -> Vec<api::perception::Detection> {
        let Some((source, camera)) = self.freshest_camera(now) else {
            return Vec::new();
        };
        let source = source.clone();
        let camera = camera.clone();
        let depth = self
            .latest_matching_depth(source.component_id(), now)
            .cloned();
        let localization = self.fresh_localization(now).cloned();
        // Detection and track association run on the frame's capture instant,
        // which rides in its envelope, so a late-arriving frame associates
        // against where the scene actually was rather than where this step is.
        let stamp = camera.at;

        let raw = self.detector.detect(DetectorInput {
            camera: &camera.body,
            depth: depth.as_ref().map(|sample| &sample.body),
            frame_id: source.frame_id.as_str(),
            stamp_ns: stamp.ticks(),
            localization: localization.as_ref(),
        });
        let mut detections = raw
            .into_iter()
            .map(|raw| raw.into_detection(source.frame_id.as_str(), localization.as_ref()))
            .collect::<Vec<_>>();
        self.tracker.update(&mut detections, stamp.ticks());
        detections
    }
}

/// Keep only the newest sample on `subscriber` that carries a production
/// instant.
///
/// A sample published without one cannot be aged against this step's clock, so
/// it is dropped rather than held as if it were current; that leaves the
/// previous slot untouched, which the freshness gate will then age out.
fn drain_latest<B: phoxal::bus::ContractBody>(
    subscriber: &Subscriber<B>,
    slot: &mut Option<Timed<B>>,
) {
    while let Some(observed) = subscriber.try_recv() {
        if let Some(at) = observed.metadata.produced_exactly_at() {
            *slot = Some(Timed::new(observed.body, at));
        }
    }
}

/// [`drain_latest`] across index-coupled subscribers and slots, one slot per
/// bound sensor.
fn drain_latest_per_source<B: phoxal::bus::ContractBody>(
    subscribers: &[Subscriber<B>],
    slots: &mut [Option<Timed<B>>],
) {
    for (subscriber, slot) in subscribers.iter().zip(slots) {
        drain_latest(subscriber, slot);
    }
}
