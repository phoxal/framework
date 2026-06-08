use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::Capability;
use phoxal::model::robot::v1::Role;
use phoxal::runtime::staged::Robot;
use tracing::warn;

/// All range-capable capabilities tagged `Role::Safety`. Empty is valid:
/// safety still publishes UnknownConservative if no near-field evidence exists.
pub(crate) fn detect_safety_range_inputs(robot: &Robot) -> Vec<CapabilityRef> {
    detect_safety_inputs(robot, |capability| {
        matches!(capability, Capability::Range(_))
    })
}

pub(crate) fn detect_safety_emergency_stop_inputs(robot: &Robot) -> Vec<CapabilityRef> {
    detect_safety_inputs(robot, |capability| {
        matches!(capability, Capability::EmergencyStop(_))
    })
}

fn detect_safety_inputs(
    robot: &Robot,
    accepts: impl Fn(&Capability) -> bool,
) -> Vec<CapabilityRef> {
    let mut inputs = Vec::new();

    for (component_id, component) in &robot.model.components {
        for (capability_id, roles) in &component.roles {
            if !roles.contains(&Role::Safety) {
                continue;
            }

            let capability_ref = CapabilityRef::new(component_id, capability_id);
            let capability = match robot.capability(&capability_ref) {
                Ok(capability) => capability,
                Err(error) => {
                    warn!(%error, capability = %capability_ref,
                        "safety runtime skipped unresolved safety capability");
                    continue;
                }
            };

            if accepts(capability) {
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
    use phoxal::runtime::staged::Robot;

    use super::{detect_safety_emergency_stop_inputs, detect_safety_range_inputs};

    #[test]
    fn detects_safety_range_inputs_from_fixture() {
        let robot = fixture_robot();
        assert_eq!(
            detect_safety_range_inputs(&robot),
            vec![CapabilityRef::new("front_center_tof", "range")]
        );
    }

    #[test]
    fn fixture_has_no_safety_emergency_stop_inputs() {
        let robot = fixture_robot();
        assert_eq!(detect_safety_emergency_stop_inputs(&robot), Vec::new());
    }

    fn fixture_robot() -> Robot {
        let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(value) => PathBuf::from(value),
            Err(error) => panic!("CARGO_MANIFEST_DIR is not set: {error}"),
        };
        let workspace_root = match manifest_dir.parent().and_then(|path| path.parent()) {
            Some(path) => path,
            None => panic!(
                "runtime/safety CARGO_MANIFEST_DIR must live two levels below the workspace root: {}",
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
}
