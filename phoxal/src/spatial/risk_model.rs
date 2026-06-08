use std::collections::HashMap;

use crate::model::robot::v1::KinematicConfig;
use crate::model::structure::Structure;
use anyhow::{Result, bail};
use nalgebra::{Isometry3, Point3};
use urdf_rs::Geometry;

use crate::spatial::frame::{extract_link_transforms, pose_to_isometry, transform_from_isometry};
use crate::spatial::geometry::convex_hull_xy;

const BASE_FOOTPRINT_LINK: &str = "base_footprint";
const BASE_LINK: &str = "base_link";

#[derive(Debug, Clone)]
pub struct SafetyModel {
    pub base_link_to_base_footprint: crate::model::robot::v1::transform::Transform,
    pub collision_primitives: Vec<SafetyCollisionPrimitive>,
    pub support_regions: Vec<SafetyRegion>,
    pub support_plane_z_m: f32,
}

#[derive(Debug, Clone)]
pub struct SafetyCollisionPrimitive {
    pub polygon_xy_m: Vec<[f32; 2]>,
    pub min_z_m: f32,
    pub max_z_m: f32,
}

#[derive(Debug, Clone)]
pub struct SafetyRegion {
    pub polygon_xy_m: Vec<[f32; 2]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyKinematicEvaluator {
    Differential,
}

impl SafetyKinematicEvaluator {
    pub fn from_kinematic(kinematic: &KinematicConfig) -> Result<Self> {
        match kinematic {
            KinematicConfig::Differential { .. } => Ok(Self::Differential),
            KinematicConfig::Mecanum { .. }
            | KinematicConfig::Ackermann { .. }
            | KinematicConfig::Omnidirectional { .. } => {
                bail!(
                    "safety has no evaluator for motion.kinematic.kind = {}",
                    kinematic.kind()
                )
            }
        }
    }
}

pub fn resolve_safety_model(
    evaluator: SafetyKinematicEvaluator,
    structure: &Structure,
) -> Result<SafetyModel> {
    match evaluator {
        SafetyKinematicEvaluator::Differential => {}
    }
    let link_transforms = extract_link_transforms(structure)?;
    let root_to_base_footprint = link_transforms
        .get(BASE_FOOTPRINT_LINK)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("structure must define '{}'", BASE_FOOTPRINT_LINK))?;
    let root_to_base_link = link_transforms
        .get(BASE_LINK)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("structure must define '{}'", BASE_LINK))?;
    let base_footprint_to_root = root_to_base_footprint.inverse();
    let base_link_to_base_footprint = base_footprint_to_root * root_to_base_link;

    let collision_primitives =
        collision_primitives(structure, &link_transforms, base_footprint_to_root)?;
    Ok(SafetyModel {
        base_link_to_base_footprint: transform_from_isometry(base_link_to_base_footprint),
        support_regions: support_regions_from_collision(&collision_primitives)?,
        collision_primitives,
        support_plane_z_m: 0.0,
    })
}

fn collision_primitives(
    structure: &Structure,
    link_transforms: &HashMap<String, Isometry3<f64>>,
    base_footprint_to_root: Isometry3<f64>,
) -> Result<Vec<SafetyCollisionPrimitive>> {
    let mut primitives = Vec::new();
    let mut mesh_count = 0;
    for link in &structure.links {
        let Some(link_transform) = link_transforms.get(&link.name) else {
            continue;
        };
        for collision in &link.collision {
            if matches!(collision.geometry, Geometry::Mesh { .. }) {
                mesh_count += 1;
                continue;
            }
            let transform =
                base_footprint_to_root * *link_transform * pose_to_isometry(&collision.origin);
            primitives.push(collision_primitive(&collision.geometry, transform)?);
        }
    }

    if primitives.is_empty() && mesh_count > 0 {
        bail!(
            "safety requires simplified deterministic collision primitives; mesh-only collision geometry is not supported"
        );
    }
    if primitives.is_empty() {
        bail!("safety requires at least one usable collision primitive");
    }
    Ok(primitives)
}

fn collision_primitive(
    geometry: &Geometry,
    transform: Isometry3<f64>,
) -> Result<SafetyCollisionPrimitive> {
    let points = match geometry {
        Geometry::Box { size } => box_corners(
            size[0] as f32 * 0.5,
            size[1] as f32 * 0.5,
            size[2] as f32 * 0.5,
        ),
        Geometry::Cylinder { radius, length } => cylinder_points(*radius as f32, *length as f32),
        Geometry::Capsule { radius, length } => {
            cylinder_points(*radius as f32, *length as f32 + *radius as f32 * 2.0)
        }
        Geometry::Sphere { radius } => cylinder_points(*radius as f32, *radius as f32 * 2.0),
        Geometry::Mesh { .. } => Vec::new(),
    };

    let transformed = points
        .into_iter()
        .map(|point| {
            transform.transform_point(&Point3::new(
                point[0] as f64,
                point[1] as f64,
                point[2] as f64,
            ))
        })
        .collect::<Vec<_>>();
    let min_z = transformed
        .iter()
        .map(|point| point.z as f32)
        .fold(f32::INFINITY, f32::min);
    let max_z = transformed
        .iter()
        .map(|point| point.z as f32)
        .fold(f32::NEG_INFINITY, f32::max);

    if !min_z.is_finite() || !max_z.is_finite() {
        bail!("safety collision primitive has invalid geometry");
    }

    let polygon_xy_m = convex_hull_xy(
        transformed
            .iter()
            .map(|point| [point.x as f32, point.y as f32])
            .collect(),
    )?;

    Ok(SafetyCollisionPrimitive {
        polygon_xy_m,
        min_z_m: min_z,
        max_z_m: max_z,
    })
}

fn support_regions_from_collision(
    collision_primitives: &[SafetyCollisionPrimitive],
) -> Result<Vec<SafetyRegion>> {
    let points = collision_primitives
        .iter()
        .flat_map(|primitive| primitive.polygon_xy_m.iter().copied())
        .collect::<Vec<_>>();
    Ok(vec![SafetyRegion {
        polygon_xy_m: convex_hull_xy(points)?,
    }])
}

fn cylinder_points(radius_m: f32, height_m: f32) -> Vec<[f32; 3]> {
    let half_height = height_m * 0.5;
    (0..16)
        .flat_map(|index| {
            let angle = index as f32 / 16.0 * std::f32::consts::TAU;
            let x = radius_m * angle.cos();
            let y = radius_m * angle.sin();
            [[x, y, -half_height], [x, y, half_height]]
        })
        .collect()
}

fn box_corners(hx: f32, hy: f32, hz: f32) -> Vec<[f32; 3]> {
    [-hx, hx]
        .into_iter()
        .flat_map(move |x| {
            [-hy, hy]
                .into_iter()
                .flat_map(move |y| [-hz, hz].into_iter().map(move |z| [x, y, z]))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::model::component::v1::CapabilityRef;
    use crate::model::robot::v1::KinematicConfig;
    use crate::model::structure::Structure;

    use super::{SafetyKinematicEvaluator, resolve_safety_model};

    const TOLERANCE: f32 = 1e-6;

    fn assert_close(left: f32, right: f32) {
        assert!(
            (left - right).abs() <= TOLERANCE,
            "expected {left} to be within {TOLERANCE} of {right}"
        );
    }

    fn assert_polygon_close(left: &[[f32; 2]], right: &[[f32; 2]]) {
        assert_eq!(left.len(), right.len());
        for (left, right) in left.iter().zip(right) {
            assert_close(left[0], right[0]);
            assert_close(left[1], right[1]);
        }
    }

    #[test]
    fn unsupported_kinematic_kind_fails_at_safety_evaluator_selection() {
        let result = SafetyKinematicEvaluator::from_kinematic(&KinematicConfig::Mecanum {
            front_left_actuator: CapabilityRef::new("front_left", "motor"),
            front_right_actuator: CapabilityRef::new("front_right", "motor"),
            rear_left_actuator: CapabilityRef::new("rear_left", "motor"),
            rear_right_actuator: CapabilityRef::new("rear_right", "motor"),
            wheel_radius_m: 0.1,
            wheel_base_m: 0.4,
            track_m: 0.3,
        });

        assert!(
            result
                .expect_err("mecanum evaluator is not implemented yet")
                .to_string()
                .contains("motion.kinematic.kind = mecanum")
        );
    }

    #[test]
    fn resolves_box_collision_origin_into_base_footprint_geometry() {
        let structure = Structure::from_urdf_str(
            r#"
            <robot name="test">
              <link name="base_footprint"/>
              <joint name="base_joint" type="fixed">
                <parent link="base_footprint"/>
                <child link="base_link"/>
                <origin xyz="0 0 0" rpy="0 0 0"/>
              </joint>
              <link name="base_link">
                <collision>
                  <origin xyz="1 -2 0.5" rpy="0 0 0"/>
                  <geometry>
                    <box size="2 4 0.6"/>
                  </geometry>
                </collision>
              </link>
            </robot>
            "#,
        )
        .expect("valid structure");

        let model = resolve_safety_model(SafetyKinematicEvaluator::Differential, &structure)
            .expect("safety model");
        let primitive = model
            .collision_primitives
            .first()
            .expect("collision primitive");

        assert_polygon_close(
            &primitive.polygon_xy_m,
            &[[0.0, -4.0], [2.0, -4.0], [2.0, 0.0], [0.0, 0.0]],
        );
        assert_close(primitive.min_z_m, 0.2);
        assert_close(primitive.max_z_m, 0.8);
        assert_polygon_close(
            &model.support_regions[0].polygon_xy_m,
            &[[0.0, -4.0], [2.0, -4.0], [2.0, 0.0], [0.0, 0.0]],
        );
    }

    #[test]
    fn resolves_identity_box_collision_at_own_extents() {
        let structure = Structure::from_urdf_str(
            r#"
            <robot name="test">
              <link name="base_footprint"/>
              <joint name="base_joint" type="fixed">
                <parent link="base_footprint"/>
                <child link="base_link"/>
                <origin xyz="0 0 0" rpy="0 0 0"/>
              </joint>
              <link name="base_link">
                <collision>
                  <geometry>
                    <box size="2 4 0.6"/>
                  </geometry>
                </collision>
              </link>
            </robot>
            "#,
        )
        .expect("valid structure");

        let model = resolve_safety_model(SafetyKinematicEvaluator::Differential, &structure)
            .expect("safety model");
        let primitive = model
            .collision_primitives
            .first()
            .expect("collision primitive");

        assert_polygon_close(
            &primitive.polygon_xy_m,
            &[[-1.0, -2.0], [1.0, -2.0], [1.0, 2.0], [-1.0, 2.0]],
        );
        assert_close(primitive.min_z_m, -0.3);
        assert_close(primitive.max_z_m, 0.3);
    }
}
