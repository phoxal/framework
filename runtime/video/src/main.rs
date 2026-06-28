//! `video` — operator preview stream lifecycle service.
//!
//! The y2026_1 video contract exposes a compact open query and stream event
//! topic. The default backend is intentionally event-only: it subscribes to raw
//! camera frames and emits `Started`, `KeyFrame`, and `Stopped` events without
//! linking a codec. A real H.264 backend belongs behind the optional `h264`
//! feature in a follow-up, not in the default dependency set.

use anyhow::{Result, anyhow};
use phoxal::api::y2026_1 as api;
use phoxal::bus::QueryFailure;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::Capability;
use phoxal::model::v1::Robot;
use phoxal::prelude::*;

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

    fn camera_topic(
        &self,
    ) -> phoxal::bus::Topic<phoxal::bus::PubSub<api::component::camera::Frame>> {
        api::topic::new()
            .component(&self.capability.component_id)
            .camera(&self.capability.capability_id)
            .frame()
    }

    fn event_topic(
        &self,
    ) -> phoxal::bus::Topic<phoxal::bus::PubSub<api::video::stream::StreamEvent>> {
        api::topic::new().video().stream(&self.stream_id).event()
    }

    fn capability_key(&self) -> String {
        self.capability.to_string()
    }
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "video", api = y2026_1)]
struct Video {
    // Runtime-private state.
    sources: Vec<VideoSource>,
    active: Vec<bool>,
    started: Vec<bool>,
    last_frame_ns: Vec<Option<u64>>,
    last_time: LogicalTime,
    // Handles.
    cameras: Vec<Subscriber<api::component::camera::Frame>>,
    events: Vec<Publisher<api::video::stream::StreamEvent>>,
}

#[phoxal::runtime]
impl Video {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let sources = build_video_sources(ctx.robot()?)?;

        let mut cameras = Vec::with_capacity(sources.len());
        let mut events = Vec::with_capacity(sources.len());
        for source in &sources {
            cameras.push(ctx.subscribe(source.camera_topic()).subscriber().await?);
            events.push(ctx.publisher(source.event_topic()).await?);
        }

        Ok(Self {
            active: vec![false; sources.len()],
            started: vec![false; sources.len()],
            last_frame_ns: vec![None; sources.len()],
            last_time: LogicalTime::new(0, 0),
            cameras,
            events,
            sources,
        })
    }

    #[step(hz = 30)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        self.last_time = step.time();

        for index in 0..self.sources.len() {
            if self.active[index] && !self.started[index] {
                self.events[index]
                    .publish_at(step.time(), api::video::stream::StreamEvent::Started)
                    .await?;
                self.started[index] = true;
            }
        }

        for index in 0..self.cameras.len() {
            while let Some(received) = self.cameras[index].try_recv() {
                let _raw_frame_identity = (
                    received.body.width,
                    received.body.height,
                    received.body.encoding,
                    received.body.measured_at_ns,
                );
                self.last_frame_ns[index] = Some(received.metadata.produced_at_ns);
                if !self.active[index] {
                    continue;
                }
                self.events[index]
                    .publish_at(step.time(), api::video::stream::StreamEvent::KeyFrame)
                    .await?;
            }
        }

        Ok(())
    }

    #[server(topic = api::topic::new().video().open())]
    async fn open(
        &mut self,
        request: api::video::OpenRequest,
    ) -> ServerResult<api::video::OpenResponse> {
        open_stream(&self.sources, &mut self.active, &mut self.started, request)
    }

    #[shutdown]
    async fn shutdown(&mut self, _ctx: ShutdownContext) -> Result<()> {
        for ((active, started), publisher) in self
            .active
            .iter_mut()
            .zip(&mut self.started)
            .zip(&self.events)
        {
            if *active || *started {
                let _ = publisher
                    .publish_at(self.last_time, api::video::stream::StreamEvent::Stopped)
                    .await;
            }
            *active = false;
            *started = false;
        }
        Ok(())
    }
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
    started: &mut [bool],
    request: api::video::OpenRequest,
) -> ServerResult<api::video::OpenResponse> {
    let index = resolve_open(&request, sources)?;
    if !active[index] {
        started[index] = false;
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

    use phoxal::api::ContractBody;
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
            "component/front_camera/camera/rgb/frame"
        );
        assert_eq!(
            rgb.event_topic().key(),
            "video/stream/front_camera_rgb/event"
        );
    }

    #[test]
    fn open_stream_activates_matching_source_and_returns_stream_id() {
        let sources = vec![source()];
        let mut active = vec![false];
        let mut started = vec![false];

        let response = open_stream(
            &sources,
            &mut active,
            &mut started,
            request("front_camera.rgb"),
        )
        .unwrap();

        assert_eq!(response.stream_id, "front_camera_rgb");
        assert_eq!(active, vec![true]);
        assert_eq!(started, vec![false]);
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
    fn emit_apis_reports_video_contracts() {
        let json = phoxal::runtime::emit_apis_json::<Video>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "video");
        assert_eq!(value["api_version"], "y2026_1");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert_contract::<api::component::camera::Frame>(contracts, "subscribe");
        assert_contract::<api::video::stream::StreamEvent>(contracts, "publish");
        assert_contract::<api::video::OpenRequest>(contracts, "server_request");
        assert_contract::<api::video::OpenResponse>(contracts, "server_response");
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
