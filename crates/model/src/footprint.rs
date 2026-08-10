//! The stock safety footprint compiled from canonical collision geometry.
//!
//! The compiler reduces every fixed collision shape to a conservative planar
//! radial envelope around `base_footprint`. A radial bound is intentionally
//! conservative: it never under-approximates a box, cylinder, capsule, or
//! sphere, and it remains valid when a fixed joint rotates a shape in three
//! dimensions. Meshes and collision geometry below movable joints are refused
//! because their runtime extent cannot be known from source-free facts alone.

use std::collections::BTreeMap;

use crate::ModelError;
use crate::component::Component;
use crate::identity::ComponentInstanceId;
use crate::robot::ComponentInstance;
use crate::structure::{Geometry, JointKind, Structure};

/// A conservative planar radial envelope around the robot's ground-projection
/// origin.
#[derive(phoxal_macros::DescribeWire, Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct FootprintEnvelope {
    /// Maximum planar distance from `base_footprint` to any collision point.
    pub radius_m: f64,
}

impl FootprintEnvelope {
    /// Construct a validated envelope.
    pub fn new(radius_m: f64) -> Result<Self, ModelError> {
        if !(radius_m.is_finite() && radius_m > 0.0) {
            return Err(ModelError::FootprintRadius);
        }
        Ok(Self { radius_m })
    }
}

impl<'de> serde::Deserialize<'de> for FootprintEnvelope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            radius_m: f64,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.radius_m).map_err(serde::de::Error::custom)
    }
}

/// Compile the robot and mounted component collision geometry into one
/// envelope. `None` means no collision geometry was authored; consumers must
/// treat that as unavailable rather than silently assuming a point robot.
pub fn compile(
    structure: &Structure,
    instances: &BTreeMap<ComponentInstanceId, ComponentInstance>,
    components: &BTreeMap<crate::identity::ComponentTypeId, Component>,
) -> Result<Option<FootprintEnvelope>, ModelError> {
    let mut radius = structure_radius(structure)?;
    for instance in instances.values() {
        let Some(component) = components.get(instance.component_type()) else {
            // Robot validation reports this separately. Keeping this path
            // explicit avoids dropping a mounted component if that invariant
            // is ever relaxed.
            continue;
        };
        let Some(component_radius) = structure_radius(component.structure())? else {
            continue;
        };
        let mount = link_transform(structure, instance.mount_link().as_str())?;
        let mount_distance = norm_xy(mount.translation);
        let candidate = mount_distance + component_radius;
        if !candidate.is_finite() {
            return Err(ModelError::FootprintNonFinite);
        }
        radius = Some(radius.map_or(candidate, |current| current.max(candidate)));
    }

    radius.map(FootprintEnvelope::new).transpose()
}

fn structure_radius(structure: &Structure) -> Result<Option<f64>, ModelError> {
    let mut max_radius = None;
    walk_links(
        structure,
        structure.root_link().as_str(),
        Transform::identity(),
        false,
        &mut max_radius,
    )?;
    Ok(max_radius)
}

fn walk_links(
    structure: &Structure,
    link_id: &str,
    transform: Transform,
    movable_ancestor: bool,
    max_radius: &mut Option<f64>,
) -> Result<(), ModelError> {
    let Some(link) = structure.link(link_id) else {
        return Err(ModelError::FootprintNonFinite);
    };
    for collision in link.collisions() {
        let shape_radius = match collision.geometry() {
            Geometry::Box { size } => half_norm(*size),
            Geometry::Cylinder { radius, length } => radial_shape_bound(*radius, *length / 2.0),
            Geometry::Capsule { radius, length } => {
                radial_shape_bound(*radius, *length / 2.0 + *radius)
            }
            Geometry::Sphere { radius } => *radius,
            Geometry::Mesh { .. } => {
                return Err(ModelError::FootprintMesh {
                    link: link.name().clone(),
                });
            }
        };
        if movable_ancestor {
            let joint = structure
                .parent_joint(link_id)
                .map(|joint| joint.name().clone());
            return Err(ModelError::FootprintMovableJoint {
                joint: joint.unwrap_or_else(|| crate::identity::JointId::new(link_id.to_string())),
            });
        }
        let shape_origin = transform.apply(collision.origin().xyz());
        let candidate = norm_xy(shape_origin) + shape_radius;
        if !candidate.is_finite() {
            return Err(ModelError::FootprintNonFinite);
        }
        *max_radius = Some(max_radius.map_or(candidate, |current| current.max(candidate)));
    }

    for joint in structure.child_joints(link_id) {
        let child_transform = transform.then(joint.origin().xyz(), joint.origin().rpy());
        let child_movable = movable_ancestor || joint.kind() != JointKind::Fixed;
        walk_links(
            structure,
            joint.child().as_str(),
            child_transform,
            child_movable,
            max_radius,
        )?;
    }
    Ok(())
}

fn link_transform(structure: &Structure, target: &str) -> Result<Transform, ModelError> {
    fn find(
        structure: &Structure,
        current: &str,
        transform: Transform,
        target: &str,
        movable_joint: Option<&crate::identity::JointId>,
    ) -> Result<Option<Transform>, ModelError> {
        if current == target {
            if let Some(joint) = movable_joint {
                return Err(ModelError::FootprintMovableJoint {
                    joint: joint.clone(),
                });
            }
            return Ok(Some(transform));
        }
        for joint in structure.child_joints(current) {
            let next_movable = movable_joint
                .or_else(|| (joint.kind() != JointKind::Fixed).then_some(joint.name()));
            if let Some(found) = find(
                structure,
                joint.child().as_str(),
                transform.then(joint.origin().xyz(), joint.origin().rpy()),
                target,
                next_movable,
            )? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
    find(
        structure,
        structure.root_link().as_str(),
        Transform::identity(),
        target,
        None,
    )
    .and_then(|transform| transform.ok_or(ModelError::FootprintNonFinite))
}

fn radial_shape_bound(radius: f64, half_length: f64) -> f64 {
    (radius * radius + half_length * half_length).sqrt()
}

fn half_norm(size: [f64; 3]) -> f64 {
    (size.into_iter().map(|value| value * value).sum::<f64>()).sqrt() / 2.0
}

fn norm_xy(value: [f64; 3]) -> f64 {
    (value[0] * value[0] + value[1] * value[1]).sqrt()
}

#[derive(Clone, Copy)]
struct Transform {
    rotation: [[f64; 3]; 3],
    translation: [f64; 3],
}

impl Transform {
    const fn identity() -> Self {
        Self {
            rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            translation: [0.0, 0.0, 0.0],
        }
    }

    fn then(self, translation: [f64; 3], rpy: [f64; 3]) -> Self {
        let local = rpy_rotation(rpy);
        let rotation = multiply(self.rotation, local);
        let offset = add(
            self.translation,
            multiply_vector(self.rotation, translation),
        );
        Self {
            rotation,
            translation: offset,
        }
    }

    fn apply(self, point: [f64; 3]) -> [f64; 3] {
        add(self.translation, multiply_vector(self.rotation, point))
    }
}

fn rpy_rotation([roll, pitch, yaw]: [f64; 3]) -> [[f64; 3]; 3] {
    let (sr, cr) = roll.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    [
        [cy * cp, cy * sp * sr - sy * cr, cy * sp * cr + sy * sr],
        [sy * cp, sy * sp * sr + cy * cr, sy * sp * cr - cy * sr],
        [-sp, cp * sr, cp * cr],
    ]
}

fn multiply(left: [[f64; 3]; 3], right: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut result = [[0.0; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            result[row][column] = (0..3)
                .map(|index| left[row][index] * right[index][column])
                .sum();
        }
    }
    result
}

fn multiply_vector(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    [
        matrix[0][0] * vector[0] + matrix[0][1] * vector[1] + matrix[0][2] * vector[2],
        matrix[1][0] * vector[0] + matrix[1][1] * vector[1] + matrix[1][2] * vector[2],
        matrix[2][0] * vector[0] + matrix[2][1] * vector[1] + matrix[2][2] * vector[2],
    ]
}

fn add(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::{FootprintEnvelope, Transform, compile, half_norm, rpy_rotation};

    fn inertial() -> Value {
        json!({
            "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
            "mass_kg": 1.0,
            "inertia": { "ixx": 1.0, "ixy": 0.0, "ixz": 0.0, "iyy": 1.0, "iyz": 0.0, "izz": 1.0 }
        })
    }

    fn link(name: &str, collisions: Value) -> Value {
        json!({
            "name": name,
            "inertial": inertial(),
            "visuals": [],
            "collisions": collisions
        })
    }

    fn fixed_joint(name: &str, parent: &str, child: &str) -> Value {
        json!({
            "name": name,
            "kind": "fixed",
            "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
            "parent": parent,
            "child": child,
            "axis": [0.0, 0.0, 1.0],
            "limit": { "lower": 0.0, "upper": 0.0, "effort": 0.0, "velocity": 0.0 }
        })
    }

    fn structure(links: Value, joints: Value) -> crate::structure::Structure {
        crate::compiler::structure(json!({
            "name": "footprint-test",
            "links": links,
            "joints": joints,
            "materials": []
        }))
        .expect("test structure is valid")
    }

    fn sphere_collision(radius: f64) -> Value {
        json!([{
            "name": "hull",
            "origin": { "xyz": [0.5, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
            "geometry": { "kind": "sphere", "radius": radius }
        }])
    }

    #[test]
    fn box_bound_is_conservative() {
        assert!((half_norm([2.0, 2.0, 2.0]) - 1.732_050_8).abs() < 1.0e-6);
    }

    #[test]
    fn fixed_transform_preserves_finite_radius() {
        let transform = Transform::identity().then([1.0, 2.0, 0.0], [0.0, 0.0, 1.0]);
        let point = transform.apply([0.5, 0.0, 0.0]);
        assert!((point[0] - (1.0 + 0.5 * 1.0_f64.cos())).abs() < 1.0e-6);
        assert!((point[1] - (2.0 + 0.5 * 1.0_f64.sin())).abs() < 1.0e-6);
        assert!(rpy_rotation([0.0, 0.0, 0.0])[0][0].is_finite());
    }

    #[test]
    fn envelope_rejects_nonfinite_values() {
        assert!(FootprintEnvelope::new(f64::NAN).is_err());
        assert!(FootprintEnvelope::new(0.0).is_err());
        assert!(
            serde_json::from_value::<FootprintEnvelope>(json!({
                "radius_m": 0.0,
                "clearance_m": 0.0
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<FootprintEnvelope>(json!({
                "radius_m": 0.2
            }))
            .is_ok()
        );
    }

    #[test]
    fn compile_reduces_fixed_collision_geometry_to_a_conservative_envelope() {
        let structure = structure(
            json!([
                link("base_footprint", sphere_collision(0.25)),
                link("base_link", json!([])),
            ]),
            json!([fixed_joint("base_joint", "base_footprint", "base_link")]),
        );
        let envelope = compile(&structure, &BTreeMap::new(), &BTreeMap::new())
            .expect("fixed geometry compiles")
            .expect("collision geometry produces an envelope");
        assert_eq!(envelope.radius_m, 0.75);
    }

    #[test]
    fn compile_rejects_meshes_and_collision_below_movable_joints() {
        let mesh = structure(
            json!([link(
                "base_footprint",
                json!([{
                    "name": "mesh",
                    "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
                    "geometry": {
                        "kind": "mesh",
                        "filename": "mesh.stl",
                        "scale": null
                    }
                }])
            )]),
            json!([]),
        );
        assert!(matches!(
            compile(&mesh, &BTreeMap::new(), &BTreeMap::new()),
            Err(crate::ModelError::FootprintMesh { .. })
        ));

        let movable = structure(
            json!([
                link("base_footprint", json!([])),
                link("base_link", json!([])),
                link("arm", sphere_collision(0.1)),
            ]),
            json!([
                fixed_joint("base_joint", "base_footprint", "base_link"),
                {
                    "name": "arm_joint",
                    "kind": "revolute",
                    "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
                    "parent": "base_link",
                    "child": "arm",
                    "axis": [0.0, 0.0, 1.0],
                    "limit": { "lower": -1.0, "upper": 1.0, "effort": 1.0, "velocity": 1.0 }
                }
            ]),
        );
        assert!(matches!(
            compile(&movable, &BTreeMap::new(), &BTreeMap::new()),
            Err(crate::ModelError::FootprintMovableJoint { .. })
        ));
    }

    #[test]
    fn compile_rejects_a_mounted_component_below_a_movable_robot_joint() {
        let robot = structure(
            json!([link("base_footprint", json!([])), link("arm", json!([])),]),
            json!([{
                "name": "arm_joint",
                "kind": "revolute",
                "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
                "parent": "base_footprint",
                "child": "arm",
                "axis": [0.0, 0.0, 1.0],
                "limit": { "lower": -1.0, "upper": 1.0, "effort": 1.0, "velocity": 1.0 }
            }]),
        );
        let component = structure(json!([link("body", sphere_collision(0.1))]), json!([]));
        let component_type = crate::identity::ComponentTypeId::new("tool")
            .expect("the test component type id is normalized");
        let instance_id = crate::identity::ComponentInstanceId::new("tool-1")
            .expect("the test component instance id is normalized");
        let instance = crate::compiler::component_instance(
            instance_id.clone(),
            component_type.clone(),
            crate::identity::LinkId::new("arm"),
            BTreeMap::new(),
        );
        let components = BTreeMap::from([(
            component_type,
            crate::compiler::component(BTreeMap::new(), component),
        )]);
        let instances = BTreeMap::from([(instance_id, instance)]);

        assert!(matches!(
            compile(&robot, &instances, &components),
            Err(crate::ModelError::FootprintMovableJoint { .. })
        ));
    }
}
