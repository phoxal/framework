//! `video` - operator preview stream lifecycle service.
//!
//! The video contract exposes a compact `video/open` query plus a per-stream
//! `state` topic. This participant enumerates the robot's camera capabilities,
//! answers `open` requests (resolving the requested capability and validating
//! dimensions against the native sensor size), subscribes to the matching raw
//! camera frames, and publishes `video/stream/<id>/state` snapshots.
//!
//! The backend is pixel-free: it publishes the stream's lifecycle `phase`
//! (`Starting` -> `Active` -> `Stopped`) and a monotonic `frames_seen` counter,
//! incremented per received source frame while active, without linking a codec
//! or encoding any pixels.

use anyhow::{Result, anyhow};
use phoxal::api;
use phoxal::bus::QueryFailure;
use phoxal::model::Robot;
use phoxal::model::component::capability::Capability;
use phoxal::model::identity::CapabilityRef;
use phoxal::prelude::*;

use api::video::stream::{StreamPhase, StreamState};

const CAMERA_STALE: std::time::Duration = std::time::Duration::from_secs(1);

/// One camera capability the operator can preview.
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

    /// Whether an `open` request naming `requested` selects this source.
    ///
    /// Three spellings are accepted for the same source: the dotted capability
    /// reference (`front_camera.rgb`), the stream id (`front_camera_rgb`), and
    /// the bare capability id (`rgb`). The bare id is ambiguous across two
    /// cameras declaring the same capability id - the first source in the
    /// robot's ordering wins - but narrowing the accepted set would reject
    /// requests that resolve today.
    fn accepts(&self, requested: &str) -> bool {
        self.capability.to_string() == requested
            || self.stream_id == requested
            || self.capability.capability_id == requested
    }

    fn validate_requested_dimensions(&self, request: &api::video::OpenRequest) -> QueryResult<()> {
        if request
            .width_px
            .is_some_and(|width| width > self.native_width_px)
            || request
                .height_px
                .is_some_and(|height| height > self.native_height_px)
        {
            return Err(QueryFailure::invalid_argument(
                "video/open dimensions must not exceed the native camera size",
            ));
        }
        Ok(())
    }
}

/// The handles bound to one preview stream.
struct StreamChannel {
    camera: Subscriber<api::component::camera::Frame>,
    state: StatePublisher<StreamState>,
}

/// What the participant latches about one preview stream between steps.
struct Stream {
    source: VideoSource,
    open: bool,
    phase: StreamPhase,
    frames_seen: u64,
    /// When the newest source frame was captured, as honestly as the camera
    /// driver could say. Absent until the first frame arrives, and absent
    /// whenever the driver could not translate its device clock into robot
    /// time.
    last_frame: Option<TimeWindow>,
}

impl Stream {
    fn new(source: VideoSource) -> Self {
        Self {
            source,
            open: false,
            phase: StreamPhase::Stopped,
            frames_seen: 0,
            last_frame: None,
        }
    }

    fn published_state(&self) -> StreamState {
        StreamState {
            phase: self.phase,
            frames_seen: self.frames_seen,
        }
    }

    /// The phase an open stream is in at `now`.
    ///
    /// A stream is `Active` only while some instant the newest capture admits
    /// is within the staleness bound and none of them is in `now`'s future. A
    /// capture the driver could not translate into robot time, or one from a
    /// world that has been replaced, is never fresh - the cross-timeline
    /// comparison has no answer, and the fail-closed reading is `Starting`.
    fn phase_at(&self, now: RobotInstant) -> StreamPhase {
        let fresh = self.last_frame.is_some_and(|captured_at| {
            captured_at
                .possibly_fresh_within(now, CAMERA_STALE)
                .unwrap_or(false)
        });
        if fresh {
            StreamPhase::Active
        } else {
            StreamPhase::Starting
        }
    }
}

pub(crate) struct Api {
    streams: Vec<StreamChannel>,
}

pub(crate) struct VideoState {
    streams: Vec<Stream>,
}

impl VideoState {
    /// Open the stream an `open` request selects, and name it back.
    ///
    /// (Re)opening a closed stream restarts its lifecycle: the next step
    /// republishes the `Starting` -> `Active` transition from a fresh frame
    /// count.
    fn open(&mut self, request: &api::video::OpenRequest) -> QueryResult<api::video::OpenResponse> {
        let stream = self.resolve_open(request)?;
        if !stream.open {
            stream.phase = StreamPhase::Stopped;
            stream.frames_seen = 0;
        }
        stream.open = true;
        Ok(api::video::OpenResponse {
            stream_id: stream.source.stream_id.clone(),
        })
    }

    fn resolve_open(&mut self, request: &api::video::OpenRequest) -> QueryResult<&mut Stream> {
        if self.streams.is_empty() {
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

        let stream = self
            .streams
            .iter_mut()
            .find(|stream| stream.source.accepts(requested))
            .ok_or_else(|| {
                QueryFailure::not_found(format!("unknown camera capability '{requested}'"))
            })?;
        stream.source.validate_requested_dimensions(request)?;
        Ok(stream)
    }
}

#[phoxal::service(state = VideoState, api = Api)]
pub(crate) struct Video;

impl Participant for Video {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let sources = video_sources(ctx.robot()?)?;

        let mut streams = Vec::with_capacity(sources.len());
        let mut channels = Vec::with_capacity(sources.len());
        for source in sources {
            channels.push(StreamChannel {
                camera: ctx.subscriber(source.camera_topic(), 32).await?,
                state: ctx.state_publisher(source.state_topic()).await?,
            });
            streams.push(Stream::new(source));
        }
        ctx.query(api::topic::owner().video().open(), Self::open)
            .await?;

        Ok((VideoState { streams }, Api { streams: channels }))
    }

    async fn reset(
        &self,
        _ctx: ResetContext,
        _api: &Self::Api,
        state: &mut Self::State,
    ) -> Result<()> {
        for stream in &mut state.streams {
            stream.phase = if stream.open {
                StreamPhase::Starting
            } else {
                StreamPhase::Stopped
            };
            stream.frames_seen = 0;
            stream.last_frame = None;
        }
        Ok(())
    }

    #[phoxal::step(hz = 30)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        for (stream, channel) in state.streams.iter_mut().zip(&api.streams) {
            let mut saw_frame = false;
            while let Some(observed) = channel.camera.try_recv() {
                stream.last_frame = observed.metadata.produced_at;
                if stream.open {
                    stream.frames_seen = stream.frames_seen.saturating_add(1);
                    saw_frame = true;
                }
            }
            if saw_frame {
                channel
                    .state
                    .publish(&step.token, stream.published_state())?;
            }
        }

        for (stream, channel) in state.streams.iter_mut().zip(&api.streams) {
            if !stream.open {
                continue;
            }
            let next = stream.phase_at(step.now());
            if stream.phase != next {
                stream.phase = next;
                channel
                    .state
                    .publish(&step.token, stream.published_state())?;
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
        state.open(&request)
    }
}

/// Every camera capability the robot declares, in the model's own order.
fn video_sources(robot: &Robot) -> Result<Vec<VideoSource>> {
    robot
        .capability_refs(|capability| matches!(capability, Capability::Camera(_)))
        .into_iter()
        .map(|capability| {
            let Some(Capability::Camera(camera)) = robot.capability(&capability) else {
                return Err(anyhow!(
                    "capability '{capability}' must reference a camera for video preview"
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

#[cfg(test)]
mod tests {

    use phoxal::bus::QueryCode;
    use phoxal::model::RobotBuilder;

    use super::*;

    fn request(capability: &str) -> api::video::OpenRequest {
        api::video::OpenRequest {
            capability: capability.to_string(),
            width_px: None,
            height_px: None,
        }
    }

    fn source() -> VideoSource {
        VideoSource::new(
            "front_camera.rgb"
                .parse::<CapabilityRef>()
                .expect("a normalized capability reference"),
            640,
            480,
        )
    }

    fn state_with(sources: Vec<VideoSource>) -> VideoState {
        VideoState {
            streams: sources.into_iter().map(Stream::new).collect(),
        }
    }

    #[test]
    fn sources_from_robot_enumerate_camera_topics() {
        // Three cameras and one depth sensor on one component: the depth
        // capability is what proves the enumeration selects cameras only.
        let robot = RobotBuilder::new("rover")
            .component_type("rgbd", |rgbd| {
                rgbd.camera("left_mono", "left_mono_link")
                    .camera("rgb", "rgb_link")
                    .camera("right_mono", "right_mono_link")
                    .depth("depth", "stereo_center_link")
            })
            .component("front_camera", "rgbd")
            .build()
            .expect("a valid robot");

        let sources = video_sources(&robot).unwrap();

        assert_eq!(sources.len(), 3);
        let rgb = sources
            .iter()
            .find(|source| source.capability.to_string() == "front_camera.rgb")
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
    fn open_activates_matching_source_and_returns_stream_id() {
        let mut state = state_with(vec![source()]);

        let response = state.open(&request("front_camera.rgb")).unwrap();

        assert_eq!(response.stream_id, "front_camera_rgb");
        let stream = &state.streams[0];
        assert!(stream.open);
        assert_eq!(stream.phase, StreamPhase::Stopped);
        assert_eq!(stream.frames_seen, 0);
    }

    /// All three spellings resolve, and all three must keep resolving.
    #[test]
    fn open_accepts_the_reference_the_stream_id_and_the_bare_capability_id() {
        for spelling in ["front_camera.rgb", "front_camera_rgb", "rgb"] {
            let mut state = state_with(vec![source()]);
            assert_eq!(
                state.open(&request(spelling)).unwrap().stream_id,
                "front_camera_rgb",
                "{spelling}"
            );
        }
    }

    #[test]
    fn open_rejects_unknown_and_empty_sources() {
        let unknown = state_with(vec![source()])
            .open(&request("rear_camera.rgb"))
            .unwrap_err();
        assert_eq!(unknown.code, QueryCode::NotFound);

        let unavailable = state_with(Vec::new())
            .open(&request("front_camera.rgb"))
            .unwrap_err();
        assert_eq!(unavailable.code, QueryCode::Unavailable);
    }

    #[test]
    fn open_rejects_zero_dimensions() {
        let err = state_with(vec![source()])
            .open(&api::video::OpenRequest {
                capability: "front_camera.rgb".to_string(),
                width_px: Some(0),
                height_px: Some(240),
            })
            .unwrap_err();

        assert_eq!(err.code, QueryCode::InvalidArgument);
    }

    #[test]
    fn open_rejects_dimensions_above_the_native_camera_size() {
        let err = state_with(vec![source()])
            .open(&api::video::OpenRequest {
                capability: "front_camera.rgb".to_string(),
                width_px: Some(1920),
                height_px: None,
            })
            .unwrap_err();

        assert_eq!(err.code, QueryCode::InvalidArgument);
    }
}
