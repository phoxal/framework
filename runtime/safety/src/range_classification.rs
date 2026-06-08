use std::collections::BTreeMap;

use crate::core::RangeSafetyClass;
use nalgebra::Vector3;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::structure::Structure;
use phoxal::runtime::staged::Robot;
use phoxal::spatial::sensor::resolve_sensor_poses_in_frame;
use tracing::warn;

use crate::selector::detect_safety_range_inputs;

const TARGET_FRAME: &str = "base_footprint";

/// Beam pitched below horizontal by roughly 15 degrees or more is a ground/cliff sensor.
pub(crate) const CLIFF_BEAM_Z_THRESHOLD: f32 = -0.25;

pub(crate) fn classify_safety_range_inputs(
    robot: &Robot,
    structure: &Structure,
) -> BTreeMap<String, RangeSafetyClass> {
    detect_safety_range_inputs(robot)
        .into_iter()
        .map(|device| {
            let source_id = range_source_id(&device);
            let safety_class = classify_range_device(robot, structure, &device);
            (source_id, safety_class)
        })
        .collect()
}

pub(crate) fn range_source_id(capability: &CapabilityRef) -> String {
    format!("{}.{}", capability.component_id, capability.capability_id)
}

fn classify_range_device(
    robot: &Robot,
    structure: &Structure,
    device: &CapabilityRef,
) -> RangeSafetyClass {
    let poses = match resolve_sensor_poses_in_frame(
        &robot.model,
        &robot.components,
        structure,
        std::slice::from_ref(device),
        TARGET_FRAME,
    ) {
        Ok(poses) => poses,
        Err(error) => {
            warn!(%error, capability = %device,
                "safety runtime defaulted unresolved range sensor to obstacle class");
            return RangeSafetyClass::Obstacle;
        }
    };

    let Some(pose) = poses.first() else {
        warn!(capability = %device,
            "safety runtime defaulted missing range sensor pose to obstacle class");
        return RangeSafetyClass::Obstacle;
    };

    let beam = pose.local_rotation * Vector3::x();
    if beam.z >= CLIFF_BEAM_Z_THRESHOLD {
        return RangeSafetyClass::Obstacle;
    }

    expected_floor_distance_m(pose.offset_xyz_m[2], beam.z)
        .map(|expected_floor_m| RangeSafetyClass::Cliff { expected_floor_m })
        .unwrap_or_else(|| {
            warn!(
                capability = %device,
                sensor_z_m = pose.offset_xyz_m[2],
                beam_z = beam.z,
                "safety runtime defaulted degenerate downward range sensor geometry to obstacle class"
            );
            RangeSafetyClass::Obstacle
        })
}

fn expected_floor_distance_m(sensor_z_m: f32, beam_z: f32) -> Option<f32> {
    if !sensor_z_m.is_finite() || !beam_z.is_finite() || sensor_z_m <= 0.0 || beam_z >= 0.0 {
        return None;
    }

    let distance_m = -sensor_z_m / beam_z;
    (distance_m.is_finite() && distance_m > 0.0).then_some(distance_m)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::core::RangeSafetyClass;
    use anyhow::Result;
    use phoxal::model::structure::Structure;
    use phoxal::scenario::helpers::{fixture_bundle_path, fixture_robot, workspace_root};

    use super::classify_safety_range_inputs;

    #[test]
    fn classifies_ground_tofs_as_cliff_and_forward_as_obstacle() -> Result<()> {
        const FIXTURE_BUNDLE: &str = "lowrate-range-diff-drive";

        let robot = fixture_robot(FIXTURE_BUNDLE)?;
        let structure = fixture_structure(FIXTURE_BUNDLE)?;
        let classes = classify_safety_range_inputs(&robot, &structure);

        assert_cliff(&classes, "ground_tof.range");
        assert_eq!(
            classes.get("front_center_tof.range"),
            Some(&RangeSafetyClass::Obstacle)
        );

        Ok(())
    }

    fn fixture_structure(fixture_bundle: &str) -> Result<Structure> {
        let workspace_root = workspace_root()?;
        let bundle_root = fixture_bundle_path(&workspace_root, fixture_bundle);
        Structure::read_from_dir(bundle_root)
    }

    fn assert_cliff(classes: &BTreeMap<String, RangeSafetyClass>, source_id: &str) {
        let safety_class = match classes.get(source_id) {
            Some(safety_class) => safety_class,
            None => panic!("{source_id} was not classified"),
        };
        let RangeSafetyClass::Cliff { expected_floor_m } = safety_class else {
            panic!("{source_id} was classified as {safety_class:?}");
        };
        assert!(
            (0.1..1.0).contains(expected_floor_m),
            "{source_id} expected floor distance {expected_floor_m} must be plausible"
        );
    }
}
