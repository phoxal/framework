use std::collections::{BTreeMap, BTreeSet};

use crate::model::component::v0::CapabilityRef;
use crate::model::component::v0::capability::Capability;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::{Robot, Role};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleAssignment {
    pub capability: CapabilityRef,
    pub roles: BTreeSet<Role>,
    pub source: RoleAssignmentSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleAssignmentSource {
    Explicit,
    Inferred,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleResolution {
    pub assignments: Vec<RoleAssignment>,
}

impl RoleResolution {
    #[must_use]
    pub fn capabilities_for(&self, role: Role) -> Vec<&CapabilityRef> {
        self.assignments
            .iter()
            .filter(|assignment| assignment.roles.contains(&role))
            .map(|assignment| &assignment.capability)
            .collect()
    }
}

pub fn resolve_roles(
    model: &Robot,
    components_by_type: &BTreeMap<String, crate::model::component::v0::Component>,
) -> Result<RoleResolution> {
    let mut errors = Vec::new();
    let mut capability_index = BTreeMap::new();

    for (component_id, component_instance) in &model.robot.components {
        match components_by_type.get(&component_instance.component) {
            Some(component) => {
                for (capability_id, capability) in &component.capabilities {
                    capability_index.insert(
                        CapabilityRef::new(component_id, capability_id),
                        capability.clone(),
                    );
                }
            }
            None => errors.push(format!(
                "component '{}' uses unstaged component type '{}'",
                component_id, component_instance.component
            )),
        }
    }

    let mut role_to_explicit_capabilities: BTreeMap<Role, Vec<CapabilityRef>> = BTreeMap::new();
    let mut assignments: BTreeMap<CapabilityRef, RoleAssignment> = BTreeMap::new();

    for (component_id, component_instance) in &model.robot.components {
        for (capability_id, roles) in &component_instance.roles {
            let capability_ref = CapabilityRef::new(component_id, capability_id);
            match capability_index.get(&capability_ref) {
                Some(capability) => {
                    let mut seen = BTreeSet::new();
                    for role in roles {
                        if !seen.insert(*role) {
                            errors.push(format!(
                                "components.{component_id}.roles.{capability_id} repeats role '{role}'"
                            ));
                        }
                        if !role_matches_capability(*role, capability) {
                            errors.push(format!(
                                "components.{component_id}.roles.{capability_id} assigns role '{role}' to {} capability",
                                capability.kind_name()
                            ));
                        }
                        role_to_explicit_capabilities
                            .entry(*role)
                            .or_default()
                            .push(capability_ref.clone());
                    }
                    assignments.insert(
                        capability_ref.clone(),
                        RoleAssignment {
                            capability: capability_ref,
                            roles: seen,
                            source: RoleAssignmentSource::Explicit,
                        },
                    );
                }
                None => errors.push(format!(
                    "components.{component_id}.roles.{capability_id} references missing capability"
                )),
            }
        }
    }

    for (role, capabilities) in &role_to_explicit_capabilities {
        if !role.allows_multiple_capabilities() && capabilities.len() > 1 {
            errors.push(format!(
                "role '{role}' is explicitly assigned to multiple capabilities: {}",
                format_capabilities(capabilities)
            ));
        }
    }

    for role in [
        Role::Localization,
        Role::Mapping,
        Role::Traversability,
        Role::Odometry,
    ] {
        if role_to_explicit_capabilities.contains_key(&role) {
            continue;
        }

        let candidates = capability_index
            .iter()
            .filter(|(_, capability)| role_matches_capability(role, capability))
            .map(|(capability_ref, _)| capability_ref.clone())
            .collect::<Vec<_>>();

        match candidates.as_slice() {
            [capability] => {
                assignments
                    .entry(capability.clone())
                    .and_modify(|assignment| {
                        assignment.roles.insert(role);
                    })
                    .or_insert_with(|| RoleAssignment {
                        capability: capability.clone(),
                        roles: [role].into_iter().collect(),
                        source: RoleAssignmentSource::Inferred,
                    });
            }
            [] => {}
            _ if role.allows_multiple_capabilities() => {}
            _ => errors.push(format!(
                "role '{role}' is ambiguous without an explicit model hint; candidates: {}",
                format_capabilities(&candidates)
            )),
        }
    }

    if errors.is_empty() {
        Ok(RoleResolution {
            assignments: assignments.into_values().collect(),
        })
    } else {
        bail!("Role resolution errors:\n{}", errors.join("\n"))
    }
}

fn role_matches_capability(role: Role, capability: &Capability) -> bool {
    match role {
        Role::Localization => matches!(
            capability,
            Capability::Depth(_)
                | Capability::Lidar(_)
                | Capability::Camera(_)
                | Capability::Gnss(_)
                | Capability::Imu(_)
        ),
        Role::Mapping | Role::Traversability => matches!(
            capability,
            Capability::Range(_) | Capability::Depth(_) | Capability::Lidar(_)
        ),
        Role::Odometry => matches!(capability, Capability::Imu(_)),
        Role::Perception => matches!(capability, Capability::Camera(_) | Capability::Depth(_)),
    }
}

fn format_capabilities(capabilities: &[CapabilityRef]) -> String {
    capabilities
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use crate::model::component::v0::capability::{
        Accelerometer, Camera, CameraMode, Capability, Depth, Gnss, Imu, Lidar, LidarOutput, Range,
        StructuralTarget,
    };

    use super::{Role, role_matches_capability};

    #[test]
    fn localization_role_matches_visual_inertial_backend_inputs() {
        assert!(role_matches_capability(
            Role::Localization,
            &Capability::Depth(Depth {
                target: link_target(),
                publish_rate_hz: 30.0,
                width_px: 640,
                height_px: 480,
                field_of_view_rad: None,
                min_range_m: None,
                max_range_m: None,
            })
        ));
        assert!(role_matches_capability(
            Role::Localization,
            &Capability::Lidar(Lidar {
                target: link_target(),
                publish_rate_hz: 10.0,
                output: LidarOutput::Points,
                min_range_m: None,
                max_range_m: None,
                horizontal_fov_rad: None,
                horizontal_resolution_rad: None,
                vertical_fov_rad: None,
                vertical_resolution_rad: None,
            })
        ));
        assert!(role_matches_capability(
            Role::Localization,
            &Capability::Camera(Camera {
                target: link_target(),
                mode: CameraMode::Rgb,
                publish_rate_hz: 30.0,
                width_px: 640,
                height_px: 480,
                field_of_view_rad: None,
            })
        ));
        assert!(role_matches_capability(
            Role::Localization,
            &Capability::Imu(Imu {
                target: link_target(),
                publish_rate_hz: 100.0,
                axes: None,
            })
        ));
        assert!(!role_matches_capability(
            Role::Localization,
            &Capability::Range(Range {
                target: link_target(),
                publish_rate_hz: 20.0,
                min_range_m: 0.03,
                max_range_m: 4.0,
                field_of_view_rad: 0.47,
            })
        ));
        assert!(!role_matches_capability(
            Role::Localization,
            &Capability::Accelerometer(Accelerometer {
                target: link_target(),
                publish_rate_hz: 100.0,
                axes: None,
            })
        ));
    }

    #[test]
    fn perception_role_matches_camera_and_depth_only() {
        assert!(role_matches_capability(
            Role::Perception,
            &Capability::Camera(Camera {
                target: link_target(),
                mode: crate::model::component::v0::capability::CameraMode::Rgb,
                publish_rate_hz: 30.0,
                width_px: 640,
                height_px: 480,
                field_of_view_rad: None,
            })
        ));
        assert!(role_matches_capability(
            Role::Perception,
            &Capability::Depth(Depth {
                target: link_target(),
                publish_rate_hz: 30.0,
                width_px: 640,
                height_px: 480,
                field_of_view_rad: None,
                min_range_m: None,
                max_range_m: None,
            })
        ));
        assert!(!role_matches_capability(
            Role::Perception,
            &Capability::Range(Range {
                target: link_target(),
                publish_rate_hz: 10.0,
                min_range_m: 0.05,
                max_range_m: 4.0,
                field_of_view_rad: 0.1,
            })
        ));
    }

    #[test]
    fn localization_role_matches_gnss() {
        assert!(role_matches_capability(
            Role::Localization,
            &Capability::Gnss(Gnss {
                target: link_target(),
                publish_rate_hz: 10.0,
                coordinate_system: Default::default(),
            })
        ));
    }

    fn link_target() -> StructuralTarget {
        StructuralTarget::Link {
            id: "sensor_link".to_string(),
        }
    }
}
