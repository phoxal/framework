use std::collections::HashMap;

use crate::model::component::v1::capability::StructuralTarget;
use crate::model::structure::Structure;
use anyhow::Result;
use nalgebra::{Isometry3, Translation3, UnitQuaternion};
use urdf_rs::{Joint, Pose};

const BASE_FOOTPRINT_LINK: &str = "base_footprint";
const BASE_LINK: &str = "base_link";

pub fn resolve_target_link<'a>(
    target: &'a StructuralTarget,
    structure: &'a Structure,
) -> Result<&'a str> {
    match target {
        StructuralTarget::Link { id } => structure
            .link(id)
            .map(|link| link.name.as_str())
            .ok_or_else(|| anyhow::anyhow!("link target '{}' does not resolve", id)),
        StructuralTarget::Joint { id } => structure
            .joint(id)
            .map(|joint| joint.child.link.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("joint target '{}' does not resolve to a child link", id)
            }),
    }
}

pub fn extract_link_transforms(structure: &Structure) -> Result<HashMap<String, Isometry3<f64>>> {
    let root_link = structure.root_link_name()?;
    let joints_by_child = structure
        .joints
        .iter()
        .map(|joint| (joint.child.link.clone(), joint))
        .collect::<HashMap<_, _>>();

    let mut transforms = HashMap::new();
    transforms.insert(root_link.to_string(), Isometry3::identity());
    for link in &structure.links {
        let transform = transform_for_link(&link.name, &joints_by_child, &mut transforms)?;
        transforms.insert(link.name.clone(), transform);
    }
    Ok(transforms)
}

pub fn support_surface_z_m(structure: &Structure) -> Result<f32> {
    let transforms = extract_link_transforms(structure)?;
    let body = transforms
        .get(BASE_LINK)
        .ok_or_else(|| anyhow::anyhow!("missing '{}' transform", BASE_LINK))?;
    let footprint = transforms
        .get(BASE_FOOTPRINT_LINK)
        .ok_or_else(|| anyhow::anyhow!("missing '{}' transform", BASE_FOOTPRINT_LINK))?;

    Ok((footprint.translation.z - body.translation.z) as f32)
}

pub fn pose_to_isometry(pose: &Pose) -> Isometry3<f64> {
    Isometry3::from_parts(
        Translation3::new(pose.xyz[0], pose.xyz[1], pose.xyz[2]),
        UnitQuaternion::from_euler_angles(pose.rpy[0], pose.rpy[1], pose.rpy[2]),
    )
}

pub(crate) fn transform_from_isometry(
    transform: Isometry3<f64>,
) -> crate::model::robot::v1::transform::Transform {
    let (roll, pitch, yaw) = transform.rotation.euler_angles();
    crate::model::robot::v1::transform::Transform::new(
        [
            transform.translation.x,
            transform.translation.y,
            transform.translation.z,
        ],
        [roll, pitch, yaw],
    )
}

fn transform_for_link(
    link_id: &str,
    joints_by_child: &HashMap<String, &Joint>,
    transforms: &mut HashMap<String, Isometry3<f64>>,
) -> Result<Isometry3<f64>> {
    if let Some(transform) = transforms.get(link_id) {
        return Ok(*transform);
    }

    let Some(joint) = joints_by_child.get(link_id) else {
        return Ok(Isometry3::identity());
    };
    let parent = transform_for_link(&joint.parent.link, joints_by_child, transforms)?;
    let local = pose_to_isometry(&joint.origin);
    let transform = parent * local;
    transforms.insert(link_id.to_string(), transform);
    Ok(transform)
}

#[cfg(test)]
mod tests {
    use crate::model::component::v1::capability::StructuralTarget;
    use crate::model::structure::Structure;

    use super::{extract_link_transforms, resolve_target_link, support_surface_z_m};

    fn sample_structure() -> Structure {
        Structure::from_urdf_str(
            r#"
            <robot name="test">
              <link name="base_link"/>
              <joint name="sensor_mount" type="fixed">
                <parent link="base_link"/>
                <child link="sensor_link"/>
                <origin xyz="1 2 0.5" rpy="0 0 1.57079632679"/>
              </joint>
              <link name="sensor_link"/>
            </robot>
            "#,
        )
        .expect("valid structure")
    }

    #[test]
    fn extract_link_transforms_follows_joint_chain() {
        let transforms = extract_link_transforms(&sample_structure()).expect("transforms");
        let sensor = transforms.get("sensor_link").expect("sensor transform");
        assert!((sensor.translation.x - 1.0).abs() < 1e-6);
        assert!((sensor.translation.y - 2.0).abs() < 1e-6);
        let (_, _, yaw_rad) = sensor.rotation.euler_angles();
        assert!((yaw_rad - std::f64::consts::FRAC_PI_2).abs() < 1e-6);
    }

    #[test]
    fn resolve_joint_target_uses_child_link() {
        let structure = sample_structure();
        let target = StructuralTarget::Joint {
            id: "sensor_mount".to_string(),
        };
        let link = resolve_target_link(&target, &structure).expect("resolved link");
        assert_eq!(link, "sensor_link");
    }

    #[test]
    fn structure_root_name_rejects_multiple_roots() {
        let structure = Structure::from_urdf_str(
            r#"
            <robot name="bad">
              <link name="a"/>
              <link name="b"/>
            </robot>
            "#,
        )
        .expect("valid structure");

        let result = extract_link_transforms(&structure);
        assert!(result.is_err());
        assert!(
            result
                .expect_err("multiple roots")
                .to_string()
                .contains("multiple root links")
        );
    }

    #[test]
    fn resolve_target_link_errors_for_missing_joint() {
        let target = StructuralTarget::Joint {
            id: "missing".to_string(),
        };
        assert!(resolve_target_link(&target, &sample_structure()).is_err());
    }

    #[test]
    fn support_surface_matches_base_footprint_height() {
        let structure = Structure::from_urdf_str(
            r#"
            <robot name="test">
              <link name="base_footprint"/>
              <joint name="base_joint" type="fixed">
                <parent link="base_footprint"/>
                <child link="base_link"/>
                <origin xyz="0 0 0.25" rpy="0 0 0"/>
              </joint>
              <link name="base_link"/>
            </robot>
            "#,
        )
        .expect("valid structure");

        let z_m = support_surface_z_m(&structure).expect("support surface");

        assert!((z_m + 0.25).abs() < 1e-5);
    }
}
