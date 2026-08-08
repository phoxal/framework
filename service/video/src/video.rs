//! `video` - operator preview stream capability query service.
//!
//! The video contract exposes a compact `video/open` query. This participant
//! enumerates the robot's camera capabilities and validates an exact source
//! request, but reports `unsupported` until a real encoded transport exists.
//! It does not subscribe to raw frames or fabricate a stream identity/lifecycle
//! from observing them.

use anyhow::{Result, anyhow};
use phoxal::api;
use phoxal::bus::QueryFailure;
use phoxal::model::Robot;
use phoxal::model::component::capability::Capability;
use phoxal::model::identity::CapabilityRef;
use phoxal::prelude::*;

/// One camera capability the operator can preview.
#[derive(Clone)]
struct VideoSource {
    capability: CapabilityRef,
    native_width_px: u32,
    native_height_px: u32,
}

impl VideoSource {
    fn new(capability: CapabilityRef, native_width_px: u32, native_height_px: u32) -> Self {
        Self {
            capability,
            native_width_px,
            native_height_px,
        }
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

pub(crate) struct Api;

pub(crate) struct VideoState {
    sources: Vec<VideoSource>,
}

impl VideoState {
    /// Validate an exact source request and report the current backend outcome.
    fn open(&self, request: &api::video::OpenRequest) -> QueryResult<api::video::OpenOutcome> {
        if self.sources.is_empty() {
            return Ok(api::video::OpenOutcome::Unavailable);
        }

        if request.width_px == Some(0) || request.height_px == Some(0) {
            return Err(QueryFailure::invalid_argument(
                "video/open dimensions must be non-zero when provided",
            ));
        }

        let requested = request
            .source
            .as_str()
            .parse::<CapabilityRef>()
            .map_err(|_| {
                QueryFailure::invalid_argument(
                    "video/open source is not a model capability reference",
                )
            })?;
        let source = self
            .sources
            .iter()
            .find(|source| source.capability == requested)
            .ok_or_else(|| {
                QueryFailure::not_found(format!("unknown camera capability '{requested}'"))
            })?;
        source.validate_requested_dimensions(request)?;

        Ok(api::video::OpenOutcome::Unsupported)
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

        ctx.query(api::topic::owner().video().open(), Self::open)?;

        Ok((VideoState { sources }, Api))
    }
}

impl Video {
    fn open(
        &self,
        _api: &Api,
        _query: QueryContext,
        request: api::video::OpenRequest,
        state: &mut VideoState,
    ) -> QueryResult<api::video::OpenOutcome> {
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

    fn request(source: &str) -> api::video::OpenRequest {
        api::video::OpenRequest {
            source: phoxal::VideoSourceRef::parse(source).expect("test source must be canonical"),
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
        VideoState { sources }
    }

    #[test]
    fn sources_from_robot_enumerate_camera_capabilities() {
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
        assert_eq!(rgb.native_width_px, 640);
        assert_eq!(rgb.native_height_px, 480);
    }

    #[test]
    fn open_reports_unsupported_without_creating_a_stream() {
        let state = state_with(vec![source()]);

        let response = state.open(&request("front_camera.rgb")).unwrap();

        assert_eq!(response, api::video::OpenOutcome::Unsupported);
    }

    #[test]
    fn typed_source_rejects_stream_and_bare_capability_aliases() {
        for alias in ["front_camera_rgb", "rgb", " front_camera.rgb "] {
            assert!(phoxal::VideoSourceRef::parse(alias).is_err(), "{alias}");
        }
    }

    #[test]
    fn open_rejects_unknown_sources_and_reports_unavailable_without_sources() {
        let unknown = state_with(vec![source()])
            .open(&request("rear_camera.rgb"))
            .unwrap_err();
        assert_eq!(unknown.code, QueryCode::NotFound);

        let unavailable = state_with(Vec::new())
            .open(&request("front_camera.rgb"))
            .unwrap();
        assert_eq!(unavailable, api::video::OpenOutcome::Unavailable);
    }

    #[test]
    fn open_rejects_zero_dimensions() {
        let err = state_with(vec![source()])
            .open(&api::video::OpenRequest {
                source: phoxal::VideoSourceRef::parse("front_camera.rgb").unwrap(),
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
                source: phoxal::VideoSourceRef::parse("front_camera.rgb").unwrap(),
                width_px: Some(1920),
                height_px: None,
            })
            .unwrap_err();

        assert_eq!(err.code, QueryCode::InvalidArgument);
    }
}
