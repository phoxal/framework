//! Robot-model-derived sensor bindings: enumerates the camera and depth
//! capabilities to subscribe to, and resolves each to its mount frame.

use anyhow::Result;
use phoxal::api;
use phoxal::model::Robot;
use phoxal::model::component::CapabilityRef;
use phoxal::model::component::capability::Capability;

#[derive(Clone)]
pub(crate) struct SensorBinding {
    pub(crate) component_id: String,
    pub(crate) capability_id: String,
    pub(crate) frame_id: String,
}

impl SensorBinding {
    fn from_ref(robot: &Robot, reference: CapabilityRef) -> Result<Self> {
        let frame_id = robot.link_target_frame(&reference)?;
        Ok(Self {
            frame_id,
            component_id: reference.component_id,
            capability_id: reference.capability_id,
        })
    }

    // Perception CONSUMES sensor frames (the camera/depth drivers own/publish
    // them), so these are the client `Subscribe` side from the public builder.
    pub(crate) fn camera_topic(
        &self,
    ) -> phoxal::bus::Topic<phoxal::bus::Subscribe<api::component::camera::Frame>> {
        api::topic::client()
            .component(&self.component_id)
            .camera(&self.capability_id)
            .frame()
    }

    pub(crate) fn depth_topic(
        &self,
    ) -> phoxal::bus::Topic<phoxal::bus::Subscribe<api::component::depth::Frame>> {
        api::topic::client()
            .component(&self.component_id)
            .depth(&self.capability_id)
            .frame()
    }
}

pub(crate) fn camera_bindings(robot: &Robot) -> Result<Vec<SensorBinding>> {
    robot
        .camera_capabilities()?
        .into_iter()
        .map(|reference| SensorBinding::from_ref(robot, reference))
        .collect()
}

pub(crate) fn depth_bindings(robot: &Robot) -> Result<Vec<SensorBinding>> {
    let mut references = Vec::new();
    for component_id in robot.component_ids() {
        let component = robot.component_for_instance(component_id)?;
        for (capability_id, capability) in component.capabilities() {
            if matches!(capability, Capability::Depth(_)) {
                references.push(CapabilityRef::new(component_id, capability_id));
            }
        }
    }
    references.sort();
    references
        .into_iter()
        .map(|reference| SensorBinding::from_ref(robot, reference))
        .collect()
}
