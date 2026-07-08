//! Robot-model-derived safety configuration: which sources are required
//! (battery is optional unless the robot declares one) and the sorted list of
//! per-component emergency-stop capability bindings to subscribe to.

use phoxal::model::component::v0::capability::Capability;
use phoxal::model::v0::Robot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RequiredSources {
    pub(crate) battery: bool,
    pub(crate) drive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmergencyStopBinding {
    pub(crate) component_id: String,
    pub(crate) capability_id: String,
}

pub(crate) fn required_sources(robot: &Robot) -> RequiredSources {
    RequiredSources {
        battery: robot_declares_battery(robot),
        drive: true,
    }
}

fn robot_declares_battery(robot: &Robot) -> bool {
    robot.manifest.components().iter().any(|(_, instance)| {
        robot
            .components
            .get(&instance.component)
            .is_some_and(|component| {
                component
                    .capabilities
                    .values()
                    .any(|capability| matches!(capability, Capability::Battery(_)))
            })
    })
}

pub(crate) fn emergency_stop_bindings(robot: &Robot) -> Vec<EmergencyStopBinding> {
    let mut bindings = robot
        .manifest
        .components()
        .iter()
        .filter_map(|(component_id, instance)| {
            robot
                .components
                .get(&instance.component)
                .map(|component| (component_id, component))
        })
        .flat_map(|(component_id, component)| {
            component
                .capabilities
                .iter()
                .filter(|(_, capability)| matches!(capability, Capability::EmergencyStop(_)))
                .map(|(capability_id, _)| EmergencyStopBinding {
                    component_id: component_id.clone(),
                    capability_id: capability_id.clone(),
                })
        })
        .collect::<Vec<_>>();
    bindings.sort_by(|left, right| {
        left.component_id
            .cmp(&right.component_id)
            .then_with(|| left.capability_id.cmp(&right.capability_id))
    });
    bindings
}
