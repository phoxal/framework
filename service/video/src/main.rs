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

const CAMERA_STALE_NS: u64 = 1_000_000_000;

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
        api::topic::new()
            .component(&self.capability.component_id)
            .camera(&self.capability.capability_id)
            .frame()
    }

    /// Video OWNS each `video/stream/{id}` node's state telemetry, so this is the
    /// owner `Publish` side from the `internal` builder, which requires the
    /// runner-minted owner capability (L2, plan #00).
    fn state_topic(
        &self,
        cap: phoxal::bus::OwnerCap,
    ) -> phoxal::bus::Topic<phoxal::bus::Publish<api::video::stream::StreamState>> {
        api::topic::internal::new(cap)
            .video()
            .stream(&self.stream_id)
            .state()
    }

    fn capability_key(&self) -> String {
        self.capability.to_string()
    }
}

use api::video::stream::{StreamPhase, StreamState};

#[derive(phoxal::Api)]
struct Api {
    cameras: Vec<Subscriber<api::component::camera::Frame>>,
    states: Vec<Publisher<StreamState>>,
    open: Server<api::video::OpenRequest, api::video::OpenResponse>,
}

#[phoxal::service(id = "video", config = ())]
struct Video {
    // Runtime-private state.
    sources: Vec<VideoSource>,
    active: Vec<bool>,
    phase: Vec<StreamPhase>,
    frames_seen: Vec<u64>,
    last_frame: Vec<Option<LogicalTime>>,
    last_time: LogicalTime,
}

impl Video {
    /// Publish the current `StreamState` snapshot for `index`.
    async fn publish_state(
        &mut self,
        api: &mut Api,
        index: usize,
        time: LogicalTime,
    ) -> Result<()> {
        api.states[index]
            .publish_at(
                time,
                StreamState {
                    phase: self.phase[index],
                    frames_seen: self.frames_seen[index],
                },
            )
            .await?;
        Ok(())
    }
}

#[phoxal::behavior]
impl Video {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        let sources = build_video_sources(ctx.robot()?)?;

        let mut cameras = Vec::with_capacity(sources.len());
        let mut states = Vec::with_capacity(sources.len());
        for source in &sources {
            cameras.push(ctx.subscriber(source.camera_topic(), 32).await?);
            states.push(ctx.publisher(source.state_topic(cap)).await?);
        }
        let open = ctx.server(api::topic::new().video().open()).await?;

        Ok((
            Self {
                active: vec![false; sources.len()],
                phase: vec![StreamPhase::Stopped; sources.len()],
                frames_seen: vec![0; sources.len()],
                last_frame: vec![None; sources.len()],
                last_time: LogicalTime::new(0, 0),
                sources,
            },
            Self::Api {
                cameras,
                states,
                open,
            },
        ))
    }

    #[step(hz = 30)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        self.last_time = step.time();
        for index in 0..api.cameras.len() {
            let mut saw_frame = false;
            while let Some(received) = api.cameras[index].try_recv() {
                let _raw_frame_identity = (
                    received.body.width,
                    received.body.height,
                    received.body.encoding,
                    received.body.measured_at_ns,
                );
                if !self.active[index] {
                    self.last_frame[index] = Some(LogicalTime::new(
                        received.metadata.epoch,
                        received.metadata.produced_at_ns,
                    ));
                    continue;
                }
                self.frames_seen[index] = self.frames_seen[index].saturating_add(1);
                self.last_frame[index] = Some(LogicalTime::new(
                    received.metadata.epoch,
                    received.metadata.produced_at_ns,
                ));
                saw_frame = true;
            }
            if saw_frame {
                self.publish_state(api, index, step.time()).await?;
            }
        }

        for index in 0..self.sources.len() {
            if self.active[index] {
                let next = if frame_is_fresh(self.last_frame[index], step.time()) {
                    StreamPhase::Active
                } else {
                    StreamPhase::Starting
                };
                if self.phase[index] != next {
                    self.phase[index] = next;
                    self.publish_state(api, index, step.time()).await?;
                }
            }
        }

        Ok(())
    }

    #[server(api = open)]
    async fn open(
        &mut self,
        api: &mut Self::Api,
        request: api::video::OpenRequest,
    ) -> ServerResult<api::video::OpenResponse> {
        let _ = api;
        open_stream(
            &self.sources,
            &mut self.active,
            &mut self.phase,
            &mut self.frames_seen,
            request,
        )
    }

    #[shutdown]
    async fn shutdown(&mut self, api: &mut Self::Api, _ctx: ShutdownContext) -> Result<()> {
        for index in 0..self.sources.len() {
            if self.active[index] || self.phase[index] != StreamPhase::Stopped {
                self.phase[index] = StreamPhase::Stopped;
                let _ = self.publish_state(api, index, self.last_time).await;
            }
            self.active[index] = false;
        }
        Ok(())
    }
}

fn frame_is_fresh(at: Option<LogicalTime>, now: LogicalTime) -> bool {
    at.is_some_and(|at| {
        at.epoch() == now.epoch()
            && at.time_ns() <= now.time_ns()
            && now.time_ns().saturating_sub(at.time_ns()) <= CAMERA_STALE_NS
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
) -> ServerResult<api::video::OpenResponse> {
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

fn resolve_open(request: &api::video::OpenRequest, sources: &[VideoSource]) -> ServerResult<usize> {
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
) -> ServerResult<()> {
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

fn main() -> phoxal::Result<()> {
    phoxal::run::<Video>()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use phoxal::bus::ContractBody;
    use phoxal::bus::QueryCode;
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

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
        // The owner topic builder requires the runner-minted `OwnerCap` (L2); the
        // test mints one directly via the doc-hidden `__mint`, standing in for the
        // runner.
        let cap = phoxal::bus::OwnerCap::__mint();
        assert_eq!(
            rgb.state_topic(cap).key(),
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

    #[test]
    fn api_reports_video_contracts() {
        assert_eq!(<Video as Participant>::ID, "video");

        let contracts = <<Video as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert_contract::<api::component::camera::Frame>(contracts, ContractRole::Subscribe);
        assert_contract::<api::video::stream::StreamState>(contracts, ContractRole::Publish);
        assert_contract::<api::video::OpenRequest>(contracts, ContractRole::Serve);
        assert_contract::<api::video::OpenResponse>(contracts, ContractRole::Serve);
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
