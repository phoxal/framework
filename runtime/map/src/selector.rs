use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::Capability;
use phoxal::model::robot::v1::Role;
use phoxal::runtime::staged::Robot;
use tracing::warn;

/// All range-capable capabilities tagged `Role::Mapping`.
///
/// Empty is a valid result: robots without mapping range sensors keep keyframe-only behavior.
pub(crate) fn detect_mapping_range_inputs(robot: &Robot) -> Vec<CapabilityRef> {
    let mut inputs = Vec::new();

    for (component_id, component) in &robot.model.components {
        for (capability_id, roles) in &component.roles {
            if !roles.contains(&Role::Mapping) {
                continue;
            }

            let capability_ref = CapabilityRef::new(component_id, capability_id);
            let capability = match robot.capability(&capability_ref) {
                Ok(capability) => capability,
                Err(error) => {
                    warn!(%error, capability = %capability_ref, "map runtime skipped unresolved mapping capability");
                    continue;
                }
            };

            if matches!(capability, Capability::Range(_)) {
                inputs.push(capability_ref);
            }
        }
    }

    inputs
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use phoxal::model::component::v1::CapabilityRef;
    use phoxal::model::robot::v1::Robot as RobotManifest;
    use phoxal::model::robot::v1::Role;
    use phoxal::runtime::staged::Robot;

    use super::detect_mapping_range_inputs;

    #[test]
    fn detects_range_inputs_from_fixture() {
        let robot = fixture_robot();

        assert_eq!(
            detect_mapping_range_inputs(&robot),
            vec![CapabilityRef::new("front_center_tof", "range")]
        );
    }

    #[test]
    fn ignores_non_range_mapping_capabilities() {
        let mut robot = fixture_robot();
        component_roles_mut(&mut robot, "front_center_tof").remove("range");
        component_roles_mut(&mut robot, "front_camera")
            .insert("depth".to_string(), vec![Role::Mapping]);

        assert_eq!(detect_mapping_range_inputs(&robot), Vec::new());
    }

    fn fixture_robot() -> Robot {
        let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(value) => PathBuf::from(value),
            Err(error) => panic!("CARGO_MANIFEST_DIR is not set: {error}"),
        };
        let workspace_root = match manifest_dir.parent().and_then(|path| path.parent()) {
            Some(path) => path,
            None => panic!(
                "runtime/map CARGO_MANIFEST_DIR must live two levels below the workspace root: {}",
                manifest_dir.display()
            ),
        };
        let bundle_root = workspace_root
            .join("fixture")
            .join("robot")
            .join("rgbd-imu-diff-drive");

        let model = match RobotManifest::read_from_dir(&bundle_root) {
            Ok(model) => model,
            Err(error) => panic!(
                "failed to read fixture robot from {}: {error:#}",
                bundle_root.display()
            ),
        };
        let components = model
            .used_component_types()
            .into_iter()
            .map(|component_type| {
                (
                    component_type.to_string(),
                    read_fixture_component(&bundle_root, component_type),
                )
            })
            .collect();

        Robot { model, components }
    }

    fn read_fixture_component(
        bundle_root: &Path,
        component_type: &str,
    ) -> phoxal::model::component::v1::Component {
        let fixture_root = match bundle_root.parent().and_then(Path::parent) {
            Some(path) => path,
            None => panic!(
                "fixture bundle root must live under fixture/robot: {}",
                bundle_root.display()
            ),
        };
        let component_root = fixture_root.join("component").join(component_type);
        match phoxal::model::component::Component::read_from_dir(&component_root) {
            Ok(component) => match component.as_v1() {
                Some(component) => component.clone(),
                None => panic!("fixture component '{component_type}' is not v1"),
            },
            Err(error) => panic!(
                "failed to read fixture component '{component_type}' from {}: {error:#}",
                component_root.display()
            ),
        }
    }

    fn component_roles_mut<'a>(
        robot: &'a mut Robot,
        component_id: &str,
    ) -> &'a mut std::collections::BTreeMap<String, Vec<Role>> {
        match robot.model.components.get_mut(component_id) {
            Some(component) => &mut component.roles,
            None => panic!("fixture missing {component_id} component instance"),
        }
    }
}
