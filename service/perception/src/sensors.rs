//! Robot-model-derived sensor bindings: enumerates the camera and depth
//! capabilities to subscribe to, and resolves each to its mount frame.

use phoxal::Result;
use phoxal::api;
use phoxal::model::Robot;
use phoxal::model::component::capability::Capability;
use phoxal::model::identity::{CapabilityRef, ComponentInstanceId, LinkId};

#[derive(Clone)]
pub(crate) struct SensorBinding {
    pub(crate) capability: CapabilityRef,
    /// The validated dotted source identity carried by perception batches.
    pub(crate) source: api::perception::SourceRef,
    /// The link the capability is mounted on, which is the frame its
    /// observations are expressed in.
    pub(crate) frame_id: LinkId,
}

impl SensorBinding {
    /// Every camera capability the robot declares, in `capability_refs` order.
    pub(crate) fn cameras(robot: &Robot) -> Result<Vec<Self>> {
        Self::bind(
            robot,
            robot.capability_refs(|capability| matches!(capability, Capability::Camera(_))),
        )
    }

    /// Every depth capability the robot declares, in `capability_refs` order.
    pub(crate) fn depths(robot: &Robot) -> Result<Vec<Self>> {
        Self::bind(
            robot,
            robot.capability_refs(|capability| matches!(capability, Capability::Depth(_))),
        )
    }

    fn bind(robot: &Robot, references: Vec<CapabilityRef>) -> Result<Vec<Self>> {
        references
            .into_iter()
            .map(|capability| -> Result<Self> {
                Ok(Self {
                    source: api::perception::SourceRef::parse(capability.to_string())?,
                    frame_id: robot.link_target_frame(&capability)?,
                    capability,
                })
            })
            .collect()
    }

    pub(crate) fn component_id(&self) -> &ComponentInstanceId {
        &self.capability.component_id
    }

    // Perception CONSUMES sensor frames (the camera/depth drivers own/publish
    // them), so these are the client `Subscribe` side from the public builder.
    pub(crate) fn camera_topic(
        &self,
    ) -> phoxal::Result<
        phoxal::bus::Topic<phoxal::bus::Subscribe<api::endpoint::component::camera::FrameEndpoint>>,
    > {
        Ok(api::topic::client()
            .component(&self.capability.component_id)?
            .camera(&self.capability.capability_id)?
            .frame())
    }

    pub(crate) fn depth_topic(
        &self,
    ) -> phoxal::Result<
        phoxal::bus::Topic<phoxal::bus::Subscribe<api::endpoint::component::depth::FrameEndpoint>>,
    > {
        Ok(api::topic::client()
            .component(&self.capability.component_id)?
            .depth(&self.capability.capability_id)?
            .frame())
    }
}

#[cfg(test)]
mod tests {
    use super::SensorBinding;
    use phoxal::model::RobotBuilder;

    #[test]
    fn sensor_bindings_from_robot_enumerate_camera_and_depth_topics() {
        // Three cameras and one depth sensor on one component, so each
        // enumeration has to select its own kind out of the same component.
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

        let cameras = SensorBinding::cameras(&robot).unwrap();
        let depths = SensorBinding::depths(&robot).unwrap();

        assert_eq!(cameras.len(), 3);
        assert_eq!(depths.len(), 1);
        assert!(cameras.iter().any(|camera| {
            camera
                .camera_topic()
                .expect("compiled camera bindings are valid key segments")
                .key()
                == "v0.1/component/front_camera/camera/rgb/frame"
        }));
        assert_eq!(
            depths[0]
                .depth_topic()
                .expect("compiled depth bindings are valid key segments")
                .key(),
            "v0.1/component/front_camera/depth/depth/frame"
        );
    }
}
