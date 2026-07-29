//! `video` - operator preview stream lifecycle service.
//!
//! The train-selected video contract exposes a compact `video/open` query plus a
//! per-stream `state` topic. This participant enumerates the robot's camera
//! capabilities, answers `open` requests (resolving the requested capability and
//! validating dimensions against the native sensor size), subscribes to the
//! matching raw camera frames, and publishes `video/stream/<id>/state` snapshots.
//!
//! The default backend is intentionally pixel-free: it publishes the stream's
//! lifecycle `phase` (`Starting` → `Active` → `Stopped`) and a monotonic
//! `frames_seen` counter (incremented per received source frame while active)
//! without linking a codec or encoding any pixels. A real H.264 backend belongs
//! behind the optional `h264` feature in a follow-up, not in the default
//! dependency set.

use anyhow::{Result, anyhow};
use phoxal::api;
use phoxal::bus::QueryFailure;
use phoxal::model::component::v0::CapabilityRef;
use phoxal::model::component::v0::capability::Capability;
use phoxal::model::v0::Robot;
use phoxal::prelude::*;

const CAMERA_STALE: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Clone)]
struct VideoSource {
    capability: CapabilityRef,
    native_width_px: u32,
    native_height_px: u32,
    stream_id: String,
}

impl VideoSource {
    fn new(capability: CapabilityRef, native_width_px: u32, native_height_px: u32) -> Self {
        let stream_id = format!("{}_{}", capability.component_id, capability.capability_id);
        Self {
            capability,
            native_width_px,
            native_height_px,
            stream_id,
        }
    }

    /// Video CONSUMES camera frames (the camera driver owns/publishes them), so
    /// this is the client `Subscribe` side from the public builder.
    fn camera_topic(
        &self,
    ) -> phoxal::bus::Topic<phoxal::bus::Subscribe<api::component::camera::Frame>> {
        api::topic::client()
            .component(&self.capability.component_id)
            .camera(&self.capability.capability_id)
            .frame()
    }

    /// Video OWNS each `video/stream/{id}` node's state telemetry, so this is the
    /// owner `Publish` side from the owner builder.
    fn state_topic(
        &self,
    ) -> phoxal::bus::Topic<phoxal::bus::Publish<api::video::stream::StreamState>> {
        api::topic::owner().video().stream(&self.stream_id).state()
    }

    fn capability_key(&self) -> String {
        self.capability.to_string()
    }
}

use api::video::stream::{StreamPhase, StreamState};

pub struct Api {
    cameras: Vec<Subscriber<api::component::camera::Frame>>,
    states: Vec<StatePublisher<StreamState>>,
}

pub struct VideoState {
    // Runtime-private state.
    sources: Vec<VideoSource>,
    active: Vec<bool>,
    phase: Vec<StreamPhase>,
    frames_seen: Vec<u64>,
    last_frame: Vec<Option<TimeWindow>>,
}

impl VideoState {
    /// Publish the current `StreamState` for `index`.
    async fn publish_state(&mut self, api: &Api, index: usize, step: StepContext) -> Result<()> {
        api.states[index].publish(
            step.token(),
            StreamState {
                phase: self.phase[index],
                frames_seen: self.frames_seen[index],
            },
        )?;
        Ok(())
    }
}

#[phoxal::service(state = VideoState, api = Api)]
pub struct Video;

impl Participant for Video {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let sources = build_video_sources(ctx.robot()?)?;

        let mut cameras = Vec::with_capacity(sources.len());
        let mut states = Vec::with_capacity(sources.len());
        for source in &sources {
            cameras.push(ctx.subscriber(source.camera_topic(), 32).await?);
            states.push(ctx.state_publisher(source.state_topic()).await?);
        }
        ctx.query(api::topic::owner().video().open(), Self::open)
            .await?;

        Ok((
            VideoState {
                active: vec![false; sources.len()],
                phase: vec![StreamPhase::Stopped; sources.len()],
                frames_seen: vec![0; sources.len()],
                last_frame: vec![None; sources.len()],
                sources,
            },
            Api { cameras, states },
        ))
    }

    async fn reset(
        &self,
        _ctx: ResetContext,
        _api: &Self::Api,
        state: &mut Self::State,
    ) -> Result<()> {
        for (active, phase) in state.active.iter().zip(&mut state.phase) {
            *phase = if *active {
                StreamPhase::Starting
            } else {
                StreamPhase::Stopped
            };
        }
        state.frames_seen.fill(0);
        state.last_frame.fill(None);
        Ok(())
    }

    #[phoxal::step(hz = 30)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        for index in 0..api.cameras.len() {
            let mut saw_frame = false;
            while let Some(observed) = api.cameras[index].try_recv() {
                let captured_at = observed.metadata.produced_at;
                if !state.active[index] {
                    state.last_frame[index] = captured_at;
                    continue;
                }
                state.frames_seen[index] = state.frames_seen[index].saturating_add(1);
                state.last_frame[index] = captured_at;
                saw_frame = true;
            }
            if saw_frame {
                state.publish_state(api, index, step).await?;
            }
        }

        for index in 0..state.sources.len() {
            if state.active[index] {
                let next = if frame_is_fresh(state.last_frame[index], step.now()) {
                    StreamPhase::Active
                } else {
                    StreamPhase::Starting
                };
                if state.phase[index] != next {
                    state.phase[index] = next;
                    state.publish_state(api, index, step).await?;
                }
            }
        }

        Ok(())
    }
}

impl Video {
    async fn open(
        &self,
        _api: &Api,
        request: api::video::OpenRequest,
        state: &mut VideoState,
    ) -> QueryResult<api::video::OpenResponse> {
        open_stream(
            &state.sources,
            &mut state.active,
            &mut state.phase,
            &mut state.frames_seen,
            request,
        )
    }
}

/// Whether the newest camera capture is recent enough to call the stream
/// active. A capture the driver could not translate into robot time, or one
/// from a replaced world, is never fresh.
fn frame_is_fresh(captured_at: Option<TimeWindow>, now: RobotInstant) -> bool {
    captured_at.is_some_and(|captured_at| {
        captured_at
            .possibly_fresh_within(now, CAMERA_STALE)
            .unwrap_or(false)
    })
}

fn build_video_sources(robot: &Robot) -> Result<Vec<VideoSource>> {
    robot
        .camera_capabilities()
        .into_iter()
        .map(|capability| {
            let Capability::Camera(camera) = robot.capability(&capability)? else {
                return Err(anyhow!(
                    "capability '{}' must reference a camera for video preview",
                    capability
                ));
            };
            Ok(VideoSource::new(
                capability,
                camera.width_px,
                camera.height_px,
            ))
        })
        .collect()
}

fn open_stream(
    sources: &[VideoSource],
    active: &mut [bool],
    phase: &mut [StreamPhase],
    frames_seen: &mut [u64],
    request: api::video::OpenRequest,
) -> QueryResult<api::video::OpenResponse> {
    let index = resolve_open(&request, sources)?;
    // (Re)opening a stream restarts its lifecycle: the next step republishes the
    // `Starting` → `Active` transition from a fresh frame count.
    if !active[index] {
        phase[index] = StreamPhase::Stopped;
        frames_seen[index] = 0;
    }
    active[index] = true;
    Ok(api::video::OpenResponse {
        stream_id: sources[index].stream_id.clone(),
    })
}

fn resolve_open(request: &api::video::OpenRequest, sources: &[VideoSource]) -> QueryResult<usize> {
    if sources.is_empty() {
        return Err(QueryFailure::unavailable("no camera sources are available"));
    }

    let requested = request.capability.trim();
    if requested.is_empty() {
        return Err(QueryFailure::invalid_argument(
            "video/open capability must not be empty",
        ));
    }
    if request.width_px == Some(0) || request.height_px == Some(0) {
        return Err(QueryFailure::invalid_argument(
            "video/open dimensions must be non-zero when provided",
        ));
    }

    let index = sources
        .iter()
        .position(|source| {
            source.capability_key() == requested
                || source.stream_id == requested
                || source.capability.capability_id == requested
        })
        .ok_or_else(|| {
            QueryFailure::not_found(format!("unknown camera capability '{requested}'"))
        })?;

    validate_requested_dimensions(request, &sources[index])?;
    Ok(index)
}

fn validate_requested_dimensions(
    request: &api::video::OpenRequest,
    source: &VideoSource,
) -> QueryResult<()> {
    if request
        .width_px
        .is_some_and(|width| width > source.native_width_px)
        || request
            .height_px
            .is_some_and(|height| height > source.native_height_px)
    {
        return Err(QueryFailure::invalid_argument(
            "video/open dimensions must not exceed the native camera size",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use phoxal::bus::QueryCode;

    use super::*;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixture/robot/rgbd-imu-diff-drive")
    }

    fn request(capability: &str) -> api::video::OpenRequest {
        api::video::OpenRequest {
            capability: capability.to_string(),
            width_px: None,
            height_px: None,
        }
    }

    fn source() -> VideoSource {
        VideoSource::new(CapabilityRef::new("front_camera", "rgb"), 640, 480)
    }

    #[test]
    fn build_sources_from_robot_enumerates_camera_topics() {
        let robot = Robot::read_from_dir(fixture()).unwrap();
        let sources = build_video_sources(&robot).unwrap();

        assert_eq!(sources.len(), 3);
        let rgb = sources
            .iter()
            .find(|source| source.capability_key() == "front_camera.rgb")
            .unwrap();
        assert_eq!(rgb.stream_id, "front_camera_rgb");
        assert_eq!(
            rgb.camera_topic().key(),
            "v0.1/component/front_camera/camera/rgb/frame"
        );
        assert_eq!(
            rgb.state_topic().key(),
            "v0.1/video/stream/front_camera_rgb/state"
        );
    }

    #[test]
    fn open_stream_activates_matching_source_and_returns_stream_id() {
        let sources = vec![source()];
        let mut active = vec![false];
        let mut phase = vec![StreamPhase::Stopped];
        let mut frames_seen = vec![0];

        let response = open_stream(
            &sources,
            &mut active,
            &mut phase,
            &mut frames_seen,
            request("front_camera.rgb"),
        )
        .unwrap();

        assert_eq!(response.stream_id, "front_camera_rgb");
        assert_eq!(active, vec![true]);
        assert_eq!(phase, vec![StreamPhase::Stopped]);
        assert_eq!(frames_seen, vec![0]);
    }

    #[test]
    fn resolve_open_rejects_unknown_and_empty_sources() {
        let unknown = resolve_open(&request("rear_camera.rgb"), &[source()]).unwrap_err();
        assert_eq!(unknown.code, QueryCode::NotFound);

        let unavailable = resolve_open(&request("front_camera.rgb"), &[]).unwrap_err();
        assert_eq!(unavailable.code, QueryCode::Unavailable);
    }

    #[test]
    fn resolve_open_rejects_zero_dimensions() {
        let err = resolve_open(
            &api::video::OpenRequest {
                capability: "front_camera.rgb".to_string(),
                width_px: Some(0),
                height_px: Some(240),
            },
            &[source()],
        )
        .unwrap_err();

        assert_eq!(err.code, QueryCode::InvalidArgument);
    }
}
