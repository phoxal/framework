//! `perception` - detector shell for camera/depth sensing.
//!
//! This participant subscribes to per-component camera and depth frames plus
//! `localize/state`, runs them through a detector head, and publishes a
//! source-captured `perception/detections` batch plus `perception/state` every
//! cycle. Detections from a fresh, confident localization are reported in the
//! `map` frame; otherwise they stay in the source sensor frame. A small point
//! tracker assigns stable `track_id`s by nearest same-class association within
//! a time/distance window.
//!
//! The default detector is honest: no model backend is linked, so it publishes
//! no detection batch and reports `BackendUnavailable` health instead of
//! treating an unprocessed frame as a valid empty result.

use phoxal::api;
use phoxal::model::identity::ComponentInstanceId;
use phoxal::prelude::*;

use crate::detector::{
    DetectionValidationError, DetectorFailure, DetectorHead, DetectorInput, PlaceholderDetector,
};
use crate::sensors::SensorBinding;
use crate::tracker::PointTracker;

use tracing::error;

const CAMERA_STALE: std::time::Duration = std::time::Duration::from_nanos(1_000_000_000);
const DEPTH_STALE: std::time::Duration = std::time::Duration::from_nanos(1_000_000_000);
const LOCALIZATION_STALE: std::time::Duration = std::time::Duration::from_nanos(1_000_000_000);
const MIN_LOCALIZATION_CONFIDENCE: f32 = 0.25;

pub(crate) struct Api {
    cameras: Vec<SampleReceiver<api::component::camera::Frame>>,
    depths: Vec<SampleReceiver<api::component::depth::Frame>>,
    localization: StateView<api::localize::LocalizationState>,
    detections: StatePublisher<api::perception::Detections>,
    state: StatePublisher<api::perception::State>,
}

pub(crate) struct PerceptionState {
    // Runtime-private state. `camera_sources` and `latest_cameras` are index
    // coupled and built together, as are `depth_sources` and `latest_depths`.
    camera_sources: Vec<SensorBinding>,
    depth_sources: Vec<SensorBinding>,
    latest_cameras: Vec<Option<Captured<api::component::camera::Frame>>>,
    latest_depths: Vec<Option<Timed<api::component::depth::Frame>>>,
    latest_localization: Option<Timed<api::localize::LocalizationState>>,
    detector: PlaceholderDetector,
    tracker: PointTracker,
}

/// A received measurement body and the capture provenance the publisher put in
/// its bus metadata. `None` is retained deliberately: it is an invalid camera
/// input, not a reason to pretend the sample was captured at the perception
/// step's current time.
#[derive(Clone)]
struct Captured<B> {
    body: B,
    captured_at: Option<TimeWindow>,
}

impl<B> Captured<B> {
    fn fresh_within(&self, now: RobotInstant, bound: std::time::Duration) -> bool {
        self.captured_at.is_some_and(|captured_at| {
            captured_at
                .possibly_fresh_within(now, bound)
                .unwrap_or(false)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CameraInput {
    Ready,
    Missing,
    Stale,
    Invalid,
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
            cameras.push(ctx.sample_receiver(source.camera_topic()?).await?);
        }

        let mut depths = Vec::with_capacity(depth_sources.len());
        for source in &depth_sources {
            depths.push(ctx.sample_receiver(source.depth_topic()?).await?);
        }

        let localization = ctx
            .state_view(api::topic::client().localize().state())
            .await?;
        // Perception OWNS the `perception` node (detections + state telemetry)
        // -> owner builder; sensor frames and `localize/state` are
        // CONSUMED via the public builder.
        let detections = ctx.state_publisher(api::topic::owner().perception().detections())?;
        let state = ctx.state_publisher(api::topic::owner().perception().state())?;

        Ok((
            PerceptionState {
                latest_cameras: vec![None; camera_sources.len()],
                latest_depths: vec![None; depth_sources.len()],
                latest_localization: None,
                detector: PlaceholderDetector,
                tracker: PointTracker::default(),
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
        Ok(())
    }

    #[phoxal::step(hz = 10)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        state.drain_inputs(api);

        let now = step.now();
        let detector = state.detector.detector_name().to_string();
        let unhealthy = |reason| api::perception::State::Unhealthy {
            detector: detector.clone(),
            reason,
        };

        // State is deliberately published on every cycle, including when all
        // cameras have disappeared. A missing or stale input is not allowed to
        // become a silent gap that downstream consumers mistake for health.
        let state_body = match state.camera_input(now) {
            CameraInput::Missing => unhealthy(api::perception::HealthReason::MissingCamera),
            CameraInput::Stale => unhealthy(api::perception::HealthReason::StaleCamera),
            CameraInput::Invalid => unhealthy(api::perception::HealthReason::InvalidCamera),
            CameraInput::Ready => match state.detect(now) {
                Ok(Some((source, captured_at, detections))) => {
                    let batch = api::perception::Detections {
                        source,
                        captured_at,
                        detections,
                    };
                    if let Err(error) = api.detections.publish(&step.token, batch) {
                        // A publication failure is terminal for this cycle. If
                        // the state channel is still available, report that
                        // loss explicitly before returning the original error.
                        let failure = unhealthy(api::perception::HealthReason::PublicationFailure);
                        return preserve_detection_publication_error(
                            error,
                            api.state.publish(&step.token, failure),
                        );
                    }
                    api::perception::State::Healthy { detector }
                }
                Ok(None) => unhealthy(api::perception::HealthReason::InvalidCamera),
                Err(error) => unhealthy(detector_health_reason(error)),
            },
        };

        api.state.publish(&step.token, state_body)?;
        Ok(())
    }
}

impl PerceptionState {
    fn drain_inputs(&mut self, api: &Api) {
        drain_capture_latest_per_source(&api.cameras, &mut self.latest_cameras);
        drain_latest_per_source(&api.depths, &mut self.latest_depths);
        if let Some(observed) = api.localization.observed()
            && let Some(at) = observed.metadata.produced_exactly_at()
        {
            self.latest_localization = Some(Timed::new(observed.body.clone(), at));
        }
    }

    /// Classify the camera input without collapsing missing, stale, and
    /// malformed provenance into one implicit return path.
    fn camera_input(&self, now: RobotInstant) -> CameraInput {
        if self.camera_sources.is_empty() {
            return CameraInput::Missing;
        }

        let mut saw_sample = false;
        let mut saw_stale = false;
        let mut saw_invalid = false;
        for sample in self.latest_cameras.iter().flatten() {
            saw_sample = true;
            if !valid_camera_frame(&sample.body) || sample.captured_at.is_none() {
                saw_invalid = true;
                continue;
            }
            if sample.fresh_within(now, CAMERA_STALE) {
                return CameraInput::Ready;
            }
            saw_stale = true;
        }

        if saw_invalid {
            CameraInput::Invalid
        } else if saw_stale {
            CameraInput::Stale
        } else if saw_sample {
            CameraInput::Invalid
        } else {
            CameraInput::Missing
        }
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
    ) -> Option<(&SensorBinding, &Captured<api::component::camera::Frame>)> {
        self.camera_sources
            .iter()
            .zip(&self.latest_cameras)
            .filter_map(|(source, sample)| sample.as_ref().map(|sample| (source, sample)))
            .filter(|(_, sample)| {
                valid_camera_frame(&sample.body) && sample.fresh_within(now, CAMERA_STALE)
            })
            .max_by_key(|(_, sample)| {
                sample
                    .captured_at
                    .map(|captured_at| captured_at.latest().ticks())
                    .unwrap_or_default()
            })
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
    fn fresh_localization(
        &self,
        now: RobotInstant,
    ) -> Result<Option<&api::localize::LocalizationState>, DetectorFailure> {
        let Some(sample) = self.latest_localization.as_ref() else {
            return Ok(None);
        };
        if ![
            sample.body.x_m,
            sample.body.y_m,
            sample.body.yaw_rad,
            f64::from(sample.body.confidence),
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            return Err(DetectorFailure::InvalidOutput(
                DetectionValidationError::NonFiniteLocalization,
            ));
        }
        if !sample.fresh_within(now, LOCALIZATION_STALE) {
            return Ok(None);
        }
        Ok((sample.body.confidence >= MIN_LOCALIZATION_CONFIDENCE).then_some(&sample.body))
    }

    fn detect(
        &mut self,
        now: RobotInstant,
    ) -> Result<
        Option<(
            api::perception::SourceRef,
            TimeWindow,
            Vec<api::perception::Detection>,
        )>,
        crate::detector::DetectorFailure,
    > {
        let Some((source, camera)) = self.freshest_camera(now) else {
            return Ok(None);
        };
        let source = source.clone();
        let camera = camera.clone();
        let Some(captured_at) = camera.captured_at else {
            return Ok(None);
        };
        let depth = self
            .latest_matching_depth(source.component_id(), now)
            .cloned();
        let localization = self.fresh_localization(now)?.cloned();
        // Detection and track association run on the frame's capture instant,
        // which rides in its envelope, so a late-arriving frame associates
        // against where the scene actually was rather than where this step is.
        let stamp = captured_at.latest();

        let raw = self.detector.detect(DetectorInput {
            camera: &camera.body,
            depth: depth.as_ref().map(|sample| &sample.body),
            frame_id: source.frame_id.as_str(),
            stamp_ns: stamp.ticks(),
            localization: localization.as_ref(),
        })?;
        let mut detections =
            checked_detections(raw, source.frame_id.as_str(), localization.as_ref())?;
        self.tracker.update(&mut detections, stamp.ticks());
        Ok(Some((source.source, captured_at, detections)))
    }
}

fn checked_detections(
    raw: Vec<crate::detector::RawDetection>,
    source_frame_id: &str,
    localization: Option<&api::localize::LocalizationState>,
) -> std::result::Result<Vec<api::perception::Detection>, DetectorFailure> {
    raw.into_iter()
        .map(|raw| raw.try_into_detection(source_frame_id, localization))
        .collect()
}

fn detector_health_reason(error: DetectorFailure) -> api::perception::HealthReason {
    match error {
        DetectorFailure::BackendUnavailable => api::perception::HealthReason::BackendUnavailable,
        DetectorFailure::Failed => api::perception::HealthReason::DetectorFailure,
        DetectorFailure::InvalidOutput(DetectionValidationError::NonFiniteLocalization) => {
            api::perception::HealthReason::InvalidCamera
        }
        DetectorFailure::InvalidOutput(_) => api::perception::HealthReason::DetectorFailure,
    }
}

/// Keep the detector publication failure as the cycle's primary error. A
/// compensating health publication is useful evidence, but a second bus error
/// must not obscure the operation that actually failed to publish detections.
fn preserve_detection_publication_error(
    detections_error: phoxal::bus::BusError,
    state_result: phoxal::bus::Result<()>,
) -> Result<()> {
    if let Err(state_error) = state_result {
        error!(
            target: "phoxal.perception",
            error = %state_error,
            "failed to publish compensating perception health state"
        );
    }
    Err(detections_error.into())
}

/// Validate the camera payload before a detector sees it. This is intentionally
/// a runtime input check: malformed measurement bytes are an unhealthy input,
/// not a fabricated empty detection set.
fn valid_camera_frame(frame: &api::component::camera::Frame) -> bool {
    if frame.width == 0 || frame.height == 0 {
        return false;
    }
    if let Some(intrinsics) = &frame.intrinsics
        && ![intrinsics.fx, intrinsics.fy, intrinsics.cx, intrinsics.cy]
            .into_iter()
            .all(f32::is_finite)
    {
        return false;
    }
    if let Some(distortion) = &frame.distortion
        && !distortion.coefficients.iter().copied().all(f32::is_finite)
    {
        return false;
    }

    let pixels = u64::from(frame.width).checked_mul(u64::from(frame.height));
    let Some(pixels) = pixels else {
        return false;
    };
    let expected = match frame.encoding {
        api::component::camera::Encoding::L8 => Some(pixels),
        api::component::camera::Encoding::Rgb8 => pixels.checked_mul(3),
        api::component::camera::Encoding::Rgba8 => pixels.checked_mul(4),
        api::component::camera::Encoding::Jpeg | api::component::camera::Encoding::Png => None,
    };
    match expected {
        Some(expected) => u64::try_from(frame.data.len()) == Ok(expected),
        None => !frame.data.is_empty(),
    }
}

/// Keep only the newest sample on `subscriber` that carries a production
/// instant.
///
/// A sample published without one cannot be aged against this step's clock, so
/// it is dropped rather than held as if it were current; that leaves the
/// previous slot untouched, which the freshness gate will then age out.
fn drain_latest<B: phoxal::bus::ContractBody + SampleDeliveryContract>(
    subscriber: &SampleReceiver<B>,
    slot: &mut Option<Timed<B>>,
) {
    while let Some(observed) = subscriber.try_recv() {
        if let Some(at) = observed.metadata.produced_exactly_at() {
            *slot = Some(Timed::new(observed.body, at));
        }
    }
}

/// Keep the newest camera sample, including an absent production window so the
/// caller can publish an explicit invalid-input health reason. The old helper
/// above remains exact-only for depth/localization consumers that still need an
/// exact robot instant for their existing math.
fn drain_capture_latest<B: phoxal::bus::ContractBody + SampleDeliveryContract>(
    subscriber: &SampleReceiver<B>,
    slot: &mut Option<Captured<B>>,
) {
    while let Some(observed) = subscriber.try_recv() {
        *slot = Some(Captured {
            body: observed.body,
            captured_at: observed.metadata.produced_at,
        });
    }
}

/// [`drain_latest`] across index-coupled subscribers and slots, one slot per
/// bound sensor.
fn drain_latest_per_source<B: phoxal::bus::ContractBody + SampleDeliveryContract>(
    subscribers: &[SampleReceiver<B>],
    slots: &mut [Option<Timed<B>>],
) {
    for (subscriber, slot) in subscribers.iter().zip(slots) {
        drain_latest(subscriber, slot);
    }
}

fn drain_capture_latest_per_source<B: phoxal::bus::ContractBody + SampleDeliveryContract>(
    subscribers: &[SampleReceiver<B>],
    slots: &mut [Option<Captured<B>>],
) {
    for (subscriber, slot) in subscribers.iter().zip(slots) {
        drain_capture_latest(subscriber, slot);
    }
}

#[cfg(test)]
mod tests {
    use phoxal::bus::{RobotInstant, TimeWindow, Timed, TimelineId};
    use phoxal::model::identity::{CapabilityId, CapabilityRef, ComponentInstanceId, LinkId};

    use super::{
        CameraInput, Captured, PerceptionState, checked_detections, detector_health_reason,
        preserve_detection_publication_error, valid_camera_frame,
    };
    use crate::detector::{
        DetectionValidationError, DetectorFailure, PlaceholderDetector, RawDetection,
    };
    use crate::sensors::SensorBinding;
    use crate::tracker::PointTracker;

    fn instant(ticks: u64) -> RobotInstant {
        RobotInstant::new(TimelineId::from_raw(1).unwrap(), ticks)
    }

    fn binding() -> SensorBinding {
        let component = ComponentInstanceId::new("front_camera").unwrap();
        let capability = CapabilityId::new("rgb").unwrap();
        SensorBinding {
            source: api::perception::SourceRef::parse("front_camera.rgb").unwrap(),
            capability: CapabilityRef::new(component, capability),
            frame_id: LinkId::new("camera_link"),
        }
    }

    fn camera() -> phoxal::api::component::camera::Frame {
        phoxal::api::component::camera::Frame {
            width: 2,
            height: 2,
            encoding: phoxal::api::component::camera::Encoding::Rgb8,
            intrinsics: None,
            distortion: None,
            exposure: None,
            calibration: None,
            data: vec![0; 12],
        }
    }

    fn state(sample: Option<Captured<phoxal::api::component::camera::Frame>>) -> PerceptionState {
        PerceptionState {
            camera_sources: vec![binding()],
            depth_sources: Vec::new(),
            latest_cameras: vec![sample],
            latest_depths: Vec::new(),
            latest_localization: None,
            detector: PlaceholderDetector,
            tracker: PointTracker::default(),
        }
    }

    #[test]
    fn camera_health_distinguishes_missing_invalid_stale_and_ready() {
        assert_eq!(
            state(None).camera_input(instant(2_000_000_000)),
            CameraInput::Missing
        );
        assert_eq!(
            state(Some(Captured {
                body: camera(),
                captured_at: None,
            }))
            .camera_input(instant(2_000_000_000)),
            CameraInput::Invalid
        );
        assert_eq!(
            state(Some(Captured {
                body: camera(),
                captured_at: Some(TimeWindow::exact(instant(0))),
            }))
            .camera_input(instant(2_000_000_000)),
            CameraInput::Stale
        );
        assert_eq!(
            state(Some(Captured {
                body: camera(),
                captured_at: Some(TimeWindow::exact(instant(1_500_000_000))),
            }))
            .camera_input(instant(2_000_000_000)),
            CameraInput::Ready
        );
    }

    #[test]
    fn placeholder_backend_does_not_produce_a_healthy_empty_batch() {
        let captured_at = TimeWindow::exact(instant(100));
        let mut state = state(Some(Captured {
            body: camera(),
            captured_at: Some(captured_at),
        }));
        let error = state.detect(instant(100 + 250_000_000)).unwrap_err();

        assert_eq!(error, DetectorFailure::BackendUnavailable);
    }

    #[test]
    fn malformed_camera_payload_is_not_treated_as_empty_detection_health() {
        let mut malformed = camera();
        malformed.data.pop();
        assert!(!valid_camera_frame(&malformed));
        assert_eq!(
            state(Some(Captured {
                body: malformed,
                captured_at: Some(TimeWindow::exact(instant(100))),
            }))
            .camera_input(instant(100)),
            CameraInput::Invalid
        );
    }

    #[test]
    fn compensating_health_failure_does_not_mask_detection_failure() {
        let error = preserve_detection_publication_error(
            phoxal::bus::BusError::Closed,
            Err(phoxal::bus::BusError::Transport(
                "health unavailable".to_string(),
            )),
        )
        .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<phoxal::bus::BusError>(),
            Some(phoxal::bus::BusError::Closed)
        ));
    }

    #[test]
    fn invalid_raw_output_produces_no_detection_batch_and_detector_health_failure() {
        let error = checked_detections(
            vec![RawDetection {
                class_id: "crate".to_string(),
                confidence: f32::NAN,
                position_m: [1.0, 2.0, 3.0],
            }],
            "camera_link",
            None,
        )
        .unwrap_err();

        assert_eq!(
            error,
            DetectorFailure::InvalidOutput(DetectionValidationError::NonFiniteConfidence)
        );
        let reason = detector_health_reason(error);
        assert_eq!(
            reason,
            phoxal::api::perception::HealthReason::DetectorFailure
        );
    }

    #[test]
    fn invalid_localization_produces_no_detection_batch_and_invalid_camera_health_failure() {
        let mut state = state(Some(Captured {
            body: camera(),
            captured_at: Some(TimeWindow::exact(instant(100))),
        }));
        state.latest_localization = Some(Timed::new(
            phoxal::api::localize::LocalizationState {
                x_m: f64::INFINITY,
                y_m: 0.0,
                yaw_rad: 0.0,
                confidence: 1.0,
            },
            instant(100),
        ));

        let error = state.detect(instant(100)).unwrap_err();
        assert_eq!(
            error,
            DetectorFailure::InvalidOutput(DetectionValidationError::NonFiniteLocalization)
        );
        assert_eq!(
            detector_health_reason(error),
            phoxal::api::perception::HealthReason::InvalidCamera
        );
    }
}
