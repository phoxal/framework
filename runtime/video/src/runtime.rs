use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow};
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, RateControlMode,
};
use openh264::formats::{RgbSliceU8, YUVBuffer};
use phoxal::api::component::v1::capability::camera;
use phoxal::api::component::v1::capability::profile::{CameraProfileEncoding, CameraProfileSpec};
use phoxal::api::video::v1::{
    Codec, EndReason, OpenRequest, OpenResponse, StreamEvent, StreamFormat, StreamPacket,
    UnavailableReason, open, stream,
};
use phoxal::bus::Bus;
use phoxal::bus::liveliness::LivelinessEvent;
use phoxal::bus::pubsub::Stamped;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::Capability;
use phoxal::model::v1::Robot;
use phoxal::runtime::clock::Step;
use phoxal::runtime::runtime::{Io, Publisher, Runtime, RuntimeInputs};
use phoxal::runtime::{EmptyArgs, QueryOptions, ReadCell, RobotRuntimeArgs};
use tracing::warn;

pub(crate) const PREVIEW_MAX_HEIGHT_PX: u32 = 480;
pub(crate) const PREVIEW_MAX_RATE_HZ: f64 = 10.0;
const H264_KEYFRAME_INTERVAL_FRAMES: u32 = 20;
const H264_TARGET_BITRATE_BPS: u32 = 750_000;

#[derive(Debug, Clone)]
pub(crate) struct PreviewSource {
    pub(crate) capability: CapabilityRef,
    pub(crate) profile_topic: String,
    pub(crate) stream_id: String,
    pub(crate) format: StreamFormat,
}

impl PreviewSource {
    pub(crate) fn new(
        capability: CapabilityRef,
        native_width_px: u32,
        native_height_px: u32,
        native_rate_hz: f64,
    ) -> Result<Self> {
        let spec = preview_spec(native_width_px, native_height_px, native_rate_hz);
        let profile_id = spec
            .to_profile_id()
            .context("failed to derive video preview profile id")?;
        let profile_topic = phoxal::api::component::v1::capability::profile_path(
            &capability.component_id,
            &capability.capability_id,
            &profile_id,
        );
        let stream_id = format!("{}_{}", capability.component_id, capability.capability_id);
        let format = StreamFormat {
            codec: Codec::H264,
            width_px: spec.width_px,
            height_px: spec.height_px,
        };

        Ok(Self {
            capability,
            profile_topic,
            stream_id,
            format,
        })
    }
}

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) sources: Vec<PreviewSource>,
}

pub(crate) enum Input {
    Frame {
        stream_id: String,
        frame: Stamped<camera::Frame>,
    },
}

pub(crate) struct VideoView {
    sources: Arc<[PreviewSource]>,
}

pub(crate) struct StreamState {
    source: PreviewSource,
    publisher: Publisher<Stamped<StreamEvent>>,
    encoder: PreviewEncoder,
    demand: Arc<AtomicUsize>,
    demand_task: tokio::task::JoinHandle<()>,
    camera_token: Option<Box<dyn Send + Sync>>,
    sequence: u64,
}

pub(crate) struct VideoRuntime {
    bus: Bus,
    view: ReadCell<VideoView>,
    streams: Vec<StreamState>,
    last_step_time_ns: u64,
}

#[async_trait::async_trait]
impl Runtime for VideoRuntime {
    const RUNTIME_ID: &'static str = "video";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Ok(Config {
            sources: build_preview_sources(&common.robot()?)?,
        })
    }

    fn clock_period(_config: &Self::Config) -> std::time::Duration {
        std::time::Duration::from_millis(33)
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        let bus = io.bus()?;
        let view = ReadCell::new(VideoView {
            sources: config.sources.into(),
        });

        io.serve_query::<OpenRequest, OpenResponse, VideoView, _>(
            &open::path(),
            view.reader(),
            QueryOptions::max_in_flight(NonZeroUsize::new(4).unwrap()),
            video_open,
        )
        .await?;

        let mut runtime = Self {
            bus,
            view,
            streams: Vec::new(),
            last_step_time_ns: 0,
        };

        let sources = runtime.view.load().sources.clone();
        runtime.streams.reserve(sources.len());
        for source in sources.iter() {
            let input_stream_id = source.stream_id.clone();
            io.subscribe::<Stamped<camera::Frame>, _>(&source.profile_topic, move |frame| {
                Input::Frame {
                    stream_id: input_stream_id.clone(),
                    frame,
                }
            })
            .await?;

            let publisher = io
                .publisher::<Stamped<StreamEvent>>(&stream::path(&source.stream_id))
                .await?;
            let demand = Arc::new(AtomicUsize::new(0));
            let demand_task = spawn_demand_task(
                runtime.bus.clone(),
                stream::path(&source.stream_id),
                Arc::clone(&demand),
            );
            let encoder = PreviewEncoder::new(source.format.width_px, source.format.height_px)?;
            runtime.streams.push(StreamState {
                source: source.clone(),
                publisher,
                encoder,
                demand,
                demand_task,
                camera_token: None,
                sequence: 0,
            });
        }

        Ok(runtime)
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        self.last_step_time_ns = step.tick.time_ns();
        for stream in &mut self.streams {
            let demanded = stream.demand.load(Ordering::Relaxed) > 0;
            if demanded && stream.camera_token.is_none() {
                let token = phoxal::bus::liveliness::declare_liveliness_token(
                    &self.bus,
                    &stream.source.profile_topic,
                )
                .await
                .map_err(|error| anyhow!("failed to declare camera demand token: {error}"))?;
                stream.camera_token = Some(Box::new(token));
                stream.encoder.request_keyframe();
                publish_stream_event(
                    stream,
                    step.tick.time_ns(),
                    StreamEvent::Opened {
                        format: stream.source.format.clone(),
                    },
                )
                .await?;
            } else if !demanded && stream.camera_token.is_some() {
                stream.camera_token = None;
                publish_stream_event(
                    stream,
                    step.tick.time_ns(),
                    StreamEvent::End {
                        reason: EndReason::Released,
                    },
                )
                .await?;
            }
        }

        for input in inputs {
            match input {
                Input::Frame { stream_id, frame } => {
                    let Some(stream) = self
                        .streams
                        .iter_mut()
                        .find(|stream| stream.source.stream_id == stream_id)
                    else {
                        continue;
                    };
                    if stream.camera_token.is_none() {
                        continue;
                    }
                    match stream.encoder.packet(stream.sequence, &frame) {
                        Ok(packet) => {
                            stream.sequence = stream.sequence.saturating_add(1);
                            publish_stream_event(
                                stream,
                                step.tick.time_ns(),
                                StreamEvent::Packet(packet),
                            )
                            .await?;
                        }
                        Err(error) => {
                            warn!(%error, "failed to encode video preview frame");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        for stream in &mut self.streams {
            if stream.camera_token.is_some() {
                publish_stream_event(
                    stream,
                    self.last_step_time_ns,
                    StreamEvent::End {
                        reason: EndReason::RuntimeStopping,
                    },
                )
                .await?;
            }
            stream.camera_token = None;
            stream.demand_task.abort();
        }
        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::runtime::ScenarioDescriptor] {
        crate::scenarios::SCENARIOS
    }

    async fn run_scenario(name: &str, common: &RobotRuntimeArgs, _args: &Self::Args) -> Result<()> {
        crate::scenarios::run(name, common).await
    }
}

async fn publish_stream_event(
    stream: &StreamState,
    timestamp_ns: u64,
    event: StreamEvent,
) -> Result<()> {
    stream
        .publisher
        .put(&Stamped::new(timestamp_ns, event))
        .await?;
    Ok(())
}

fn spawn_demand_task(
    bus: Bus,
    stream_topic: String,
    demand: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let subscriber =
            match phoxal::bus::liveliness::liveliness_subscriber(&bus, &stream_topic).await {
                Ok(subscriber) => subscriber,
                Err(error) => {
                    warn!(
                        stream_topic,
                        error = %error,
                        "failed to subscribe to video stream demand liveliness"
                    );
                    return;
                }
            };

        loop {
            match subscriber.recv().await {
                Ok(LivelinessEvent::Alive(_)) => {
                    demand.fetch_add(1, Ordering::Relaxed);
                }
                Ok(LivelinessEvent::Dropped(_)) => decrement_demand(&demand),
                Err(error) => {
                    warn!(
                        stream_topic,
                        error = %error,
                        "video stream demand liveliness subscription stopped"
                    );
                    return;
                }
            }
        }
    })
}

fn decrement_demand(demand: &AtomicUsize) {
    let _ = demand.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        current.checked_sub(1)
    });
}

pub(crate) fn build_preview_sources(robot: &Robot) -> Result<Vec<PreviewSource>> {
    robot
        .camera_capabilities()
        .into_iter()
        .map(|capability_ref| {
            let Capability::Camera(camera) = robot.capability(&capability_ref)? else {
                return Err(anyhow!(
                    "capability '{}' must reference a camera for video preview",
                    capability_ref
                ));
            };
            PreviewSource::new(
                capability_ref,
                camera.width_px,
                camera.height_px,
                camera.publish_rate_hz,
            )
        })
        .collect()
}

pub(crate) fn preview_spec(
    native_width_px: u32,
    native_height_px: u32,
    native_rate_hz: f64,
) -> CameraProfileSpec {
    let height_px = even_floor_at_least_two(native_height_px.min(PREVIEW_MAX_HEIGHT_PX));
    let width_px = if native_height_px == 0 {
        native_width_px
    } else {
        (native_width_px as u64 * height_px as u64 / native_height_px as u64) as u32
    };
    let width_px = even_floor_at_least_two(width_px);
    let publish_rate_hz = native_rate_hz.min(PREVIEW_MAX_RATE_HZ).floor().max(1.0);

    CameraProfileSpec {
        width_px,
        height_px,
        publish_rate_hz,
        encoding: CameraProfileEncoding::Rgb8,
    }
}

fn even_floor_at_least_two(value: u32) -> u32 {
    value.max(2) & !1
}

pub(crate) struct PreviewEncoder {
    encoder: Encoder,
    yuv: YUVBuffer,
    width_px: u32,
    height_px: u32,
    force_next_keyframe: bool,
    has_encoded_frame: bool,
}

impl PreviewEncoder {
    pub(crate) fn new(width_px: u32, height_px: u32) -> Result<Self> {
        if width_px < 2
            || height_px < 2
            || !width_px.is_multiple_of(2)
            || !height_px.is_multiple_of(2)
        {
            return Err(anyhow!(
                "h264 preview dimensions must be even and at least 2 px, received {}x{}",
                width_px,
                height_px
            ));
        }

        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(H264_TARGET_BITRATE_BPS))
            .max_frame_rate(FrameRate::from_hz(PREVIEW_MAX_RATE_HZ as f32))
            .rate_control_mode(RateControlMode::Bitrate)
            .intra_frame_period(IntraFramePeriod::from_num_frames(
                H264_KEYFRAME_INTERVAL_FRAMES,
            ));
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), config)
            .context("failed to create h264 preview encoder")?;

        Ok(Self {
            encoder,
            yuv: YUVBuffer::new(width_px as usize, height_px as usize),
            width_px,
            height_px,
            force_next_keyframe: false,
            has_encoded_frame: false,
        })
    }

    pub(crate) fn request_keyframe(&mut self) {
        self.force_next_keyframe = true;
    }

    pub(crate) fn encode(&mut self, frame: &camera::Frame) -> Result<Vec<u8>> {
        if frame.encoding() != camera::Encoding::Rgb8 {
            return Err(anyhow!(
                "video preview supports only rgb8 camera frames, received {:?}",
                frame.encoding()
            ));
        }
        if frame.width() != self.width_px || frame.height() != self.height_px {
            return Err(anyhow!(
                "h264 preview frame dimensions {}x{} do not match encoder dimensions {}x{}",
                frame.width(),
                frame.height(),
                self.width_px,
                self.height_px
            ));
        }

        let expected_len = expected_rgb8_len(self.width_px, self.height_px)?;
        if frame.data().len() != expected_len {
            return Err(anyhow!(
                "rgb8 preview frame has {} bytes, expected {} bytes for {}x{}",
                frame.data().len(),
                expected_len,
                self.width_px,
                self.height_px
            ));
        }

        let rgb = RgbSliceU8::new(
            frame.data(),
            (self.width_px as usize, self.height_px as usize),
        );
        self.yuv.read_rgb8(rgb);
        if self.force_next_keyframe && self.has_encoded_frame {
            self.encoder.force_intra_frame();
        }
        let bytes = self
            .encoder
            .encode(&self.yuv)
            .context("failed to encode h264 preview frame")?
            .to_vec();
        self.has_encoded_frame = true;
        self.force_next_keyframe = false;
        Ok(bytes)
    }

    pub(crate) fn packet(
        &mut self,
        sequence: u64,
        frame: &Stamped<camera::Frame>,
    ) -> Result<StreamPacket> {
        Ok(StreamPacket {
            sequence,
            captured_at_ns: frame.timestamp_ns,
            bytes: self.encode(&frame.data)?,
        })
    }
}

fn expected_rgb8_len(width_px: u32, height_px: u32) -> Result<usize> {
    let pixels = (width_px as usize)
        .checked_mul(height_px as usize)
        .context("rgb8 preview frame dimensions overflow")?;
    pixels
        .checked_mul(3)
        .context("rgb8 preview frame byte length overflows")
}

pub(crate) fn resolve_open(request_source: &str, sources: &[PreviewSource]) -> OpenResponse {
    if sources.is_empty() {
        return OpenResponse::Unavailable(UnavailableReason::NoCamerasAvailable);
    }

    match sources
        .iter()
        .find(|source| source.capability.to_string() == request_source)
    {
        Some(source) => OpenResponse::Ok {
            stream_id: source.stream_id.clone(),
            format: source.format.clone(),
        },
        None => OpenResponse::UnknownSource,
    }
}

fn video_open(view: &VideoView, request: OpenRequest) -> OpenResponse {
    resolve_open(&request.source, &view.sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::api::video::v1::Quality;

    fn preview_source() -> PreviewSource {
        PreviewSource::new(CapabilityRef::new("front_camera", "rgb"), 640, 480, 30.0)
            .expect("preview source should build")
    }

    #[test]
    fn preview_spec_caps_height_and_rate() {
        let spec = preview_spec(640, 480, 30.0);

        assert_eq!(spec.width_px, 640);
        assert_eq!(spec.height_px, 480);
        assert_eq!(spec.publish_rate_hz, 10.0);
        assert_eq!(spec.to_profile_id().unwrap().as_ref(), "r640x480_h10_rgb8");
    }

    #[test]
    fn preview_spec_preserves_smaller_source_resolution() {
        let spec = preview_spec(320, 240, 15.0);

        assert_eq!(spec.width_px, 320);
        assert_eq!(spec.height_px, 240);
        assert_eq!(spec.publish_rate_hz, 10.0);
        assert_eq!(spec.to_profile_id().unwrap().as_ref(), "r320x240_h10_rgb8");
    }

    #[test]
    fn preview_spec_scales_wide_sources_by_height() {
        let spec = preview_spec(1280, 720, 30.0);

        assert_eq!(spec.width_px, 852);
        assert_eq!(spec.height_px, 480);
        assert_eq!(spec.to_profile_id().unwrap().as_ref(), "r852x480_h10_rgb8");
    }

    #[test]
    fn h264_encoder_encodes_rgb8_frames_as_annex_b_access_units() {
        let frame = rgb8_frame(16, 16);
        let mut encoder = PreviewEncoder::new(16, 16).expect("encoder should build");

        let bytes = encoder.encode(&frame).expect("rgb8 frame should encode");

        assert!(!bytes.is_empty());
        assert!(starts_with_annex_b_start_code(&bytes));
    }

    #[test]
    fn h264_encoder_rejects_non_rgb8_frames() {
        let frame = camera::Frame::new(16, 16, camera::Encoding::L8, vec![42; 16 * 16]);
        let mut encoder = PreviewEncoder::new(16, 16).expect("encoder should build");

        assert!(encoder.encode(&frame).is_err());
    }

    #[test]
    fn h264_encoder_rejects_frame_dimension_mismatch() {
        let frame = rgb8_frame(18, 16);
        let mut encoder = PreviewEncoder::new(16, 16).expect("encoder should build");

        assert!(encoder.encode(&frame).is_err());
    }

    #[test]
    fn stream_packet_uses_source_capture_timestamp() {
        let capture_time_ns = 123_456_789;
        let frame = Stamped::new(capture_time_ns, rgb8_frame(16, 16));
        let mut encoder = PreviewEncoder::new(16, 16).expect("encoder should build");

        let packet = encoder.packet(7, &frame).expect("packet should build");

        assert_eq!(packet.sequence, 7);
        assert_eq!(packet.captured_at_ns, capture_time_ns);
        assert!(!packet.bytes.is_empty());
    }

    #[test]
    fn resolve_open_reports_unavailable_without_cameras() {
        assert_eq!(
            resolve_open("front_camera.rgb", &[]),
            OpenResponse::Unavailable(UnavailableReason::NoCamerasAvailable)
        );
    }

    #[test]
    fn resolve_open_rejects_unknown_source() {
        let source = preview_source();

        assert_eq!(
            resolve_open("rear_camera.rgb", &[source]),
            OpenResponse::UnknownSource
        );
    }

    #[test]
    fn resolve_open_returns_stream_id_and_format_for_matching_source() {
        let sources = vec![
            preview_source(),
            PreviewSource::new(CapabilityRef::new("rear_camera", "rgb"), 320, 240, 15.0)
                .expect("preview source should build"),
        ];

        assert_eq!(
            resolve_open("rear_camera.rgb", &sources),
            OpenResponse::Ok {
                stream_id: "rear_camera_rgb".to_string(),
                format: StreamFormat {
                    codec: Codec::H264,
                    width_px: 320,
                    height_px: 240,
                },
            }
        );
    }

    #[test]
    fn video_open_resolves_ok_unknown_and_unavailable() {
        let source = preview_source();
        let view = VideoView {
            sources: Arc::from(vec![source]),
        };

        assert_eq!(
            video_open(&view, OpenRequest::new("front_camera.rgb", Quality::Auto)),
            OpenResponse::Ok {
                stream_id: "front_camera_rgb".to_string(),
                format: StreamFormat {
                    codec: Codec::H264,
                    width_px: 640,
                    height_px: 480,
                },
            }
        );
        assert_eq!(
            video_open(&view, OpenRequest::new("rear_camera.rgb", Quality::Auto)),
            OpenResponse::UnknownSource
        );

        let empty_view = VideoView {
            sources: Arc::from(Vec::new()),
        };
        assert_eq!(
            video_open(
                &empty_view,
                OpenRequest::new("front_camera.rgb", Quality::Auto)
            ),
            OpenResponse::Unavailable(UnavailableReason::NoCamerasAvailable)
        );
    }

    #[test]
    fn preview_source_derives_profile_topic_stream_id_and_format() {
        let source = PreviewSource::new(CapabilityRef::new("front_camera", "rgb"), 1280, 720, 30.0)
            .expect("preview source should build");

        assert_eq!(
            source.profile_topic,
            "component/front_camera/rgb/profile/r852x480_h10_rgb8"
        );
        assert_eq!(source.stream_id, "front_camera_rgb");
        assert_eq!(
            source.format,
            StreamFormat {
                codec: Codec::H264,
                width_px: 852,
                height_px: 480,
            }
        );
    }

    fn rgb8_frame(width_px: u32, height_px: u32) -> camera::Frame {
        let mut data = Vec::with_capacity((width_px * height_px * 3) as usize);
        for y in 0..height_px {
            for x in 0..width_px {
                data.push((x % 256) as u8);
                data.push((y % 256) as u8);
                data.push(((x + y) % 256) as u8);
            }
        }
        camera::Frame::new(width_px, height_px, camera::Encoding::Rgb8, data)
    }

    fn starts_with_annex_b_start_code(bytes: &[u8]) -> bool {
        bytes.starts_with(&[0x00, 0x00, 0x00, 0x01]) || bytes.starts_with(&[0x00, 0x00, 0x01])
    }
}
