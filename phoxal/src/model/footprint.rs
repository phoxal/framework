//! The stock safety footprint compiled from canonical collision geometry.
//!
//! The compiler reduces collision shapes to a conservative planar radial
//! envelope around `base_footprint`. A radial bound is intentionally
//! conservative: it never under-approximates a box, cylinder, capsule, or
//! sphere, and it remains valid when a fixed joint rotates a shape in three
//! dimensions. Meshes and general collision geometry below movable joints are
//! refused because their runtime extent cannot be known from source-free facts
//! alone. The narrow exception is a mounted component's direct, axis-centered
//! sphere, cylinder, or capsule whose geometry is unchanged by its revolute or
//! continuous joint; that proof preserves the general movable-joint refusal.

use std::collections::BTreeMap;

use crate::model::ModelError;
use crate::model::component::Component;
use crate::model::geometry::Geometry;
use crate::model::identity::ComponentInstanceId;
use crate::model::robot::ComponentInstance;
use crate::model::structure::{Collision, Joint, JointKind, Structure};

const AXIS_INVARIANCE_TOLERANCE: f64 = 1.0e-9;

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
    components: &BTreeMap<crate::model::identity::ComponentTypeId, Component>,
) -> Result<Option<FootprintEnvelope>, ModelError> {
    let mut radius = structure_radius(structure, MovableCollisionPolicy::Reject)?;
    for instance in instances.values() {
        let Some(component) = components.get(instance.component_type()) else {
            // Robot validation reports this separately. Keeping this path
            // explicit avoids dropping a mounted component if that invariant
            // is ever relaxed.
            continue;
        };
        let Some(component_radius) = structure_radius(
            component.structure(),
            MovableCollisionPolicy::AllowDirectAxisInvariantRotation,
        )?
        else {
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum MovableCollisionPolicy {
    Reject,
    AllowDirectAxisInvariantRotation,
}

fn structure_radius(
    structure: &Structure,
    policy: MovableCollisionPolicy,
) -> Result<Option<f64>, ModelError> {
    let mut max_radius = None;
    walk_links(
        structure,
        structure.root_link().as_str(),
        Transform::identity(),
        None,
        policy,
        &mut max_radius,
    )?;
    Ok(max_radius)
}

fn walk_links<'a>(
    structure: &'a Structure,
    link_id: &str,
    transform: Transform,
    movable_ancestor: Option<&'a Joint>,
    policy: MovableCollisionPolicy,
    max_radius: &mut Option<f64>,
) -> Result<(), ModelError> {
    let Some(link) = structure.link(link_id) else {
        return Err(ModelError::FootprintNonFinite);
    };
    for collision in link.collisions() {
        let shape_radius = collision_shape_radius(collision.geometry(), link.name())?;
        let shape_origin = transform.apply(collision.origin().xyz());
        let candidate = if let Some(joint) = movable_ancestor {
            let direct_child = structure
                .parent_joint(link_id)
                .is_some_and(|parent| parent.name() == joint.name());
            if policy != MovableCollisionPolicy::AllowDirectAxisInvariantRotation
                || !direct_child
                || !rotation_keeps_collision_fixed(joint, collision)
            {
                return Err(ModelError::FootprintMovableJoint {
                    joint: joint.name().clone(),
                });
            }
            // The collision is fixed as the joint turns. Use a full 3D bound
            // around the component mount because the robot may mount this
            // component at any fixed orientation.
            norm_xyz(shape_origin) + shape_radius
        } else {
            norm_xy(shape_origin) + shape_radius
        };
        if !candidate.is_finite() {
            return Err(ModelError::FootprintNonFinite);
        }
        *max_radius = Some(max_radius.map_or(candidate, |current| current.max(candidate)));
    }

    for joint in structure.child_joints(link_id) {
        let child_transform = transform.then(joint.origin().xyz(), joint.origin().rpy());
        let child_movable =
            movable_ancestor.or_else(|| (joint.kind() != JointKind::Fixed).then_some(joint));
        walk_links(
            structure,
            joint.child().as_str(),
            child_transform,
            child_movable,
            policy,
            max_radius,
        )?;
    }
    Ok(())
}

fn rotation_keeps_collision_fixed(joint: &Joint, collision: &Collision) -> bool {
    if !matches!(joint.kind(), JointKind::Revolute | JointKind::Continuous) {
        return false;
    }
    let Some(axis) = normalized(joint.axis()) else {
        return false;
    };
    let center = collision.origin().xyz();
    let center_on_axis =
        norm_xyz(subtract(center, scale(axis, dot(center, axis)))) <= AXIS_INVARIANCE_TOLERANCE;
    if !center_on_axis {
        return false;
    }
    match collision.geometry() {
        Geometry::Sphere { .. } => true,
        Geometry::Cylinder { .. } | Geometry::Capsule { .. } => {
            let geometry_axis =
                multiply_vector(rpy_rotation(collision.origin().rpy()), [0.0, 0.0, 1.0]);
            normalized(geometry_axis).is_some_and(|geometry_axis| {
                (dot(axis, geometry_axis).abs() - 1.0).abs() <= AXIS_INVARIANCE_TOLERANCE
            })
        }
        Geometry::Box { .. } | Geometry::Mesh { .. } => false,
    }
}

fn collision_shape_radius(
    geometry: &Geometry,
    link: &crate::model::identity::LinkId,
) -> Result<f64, ModelError> {
    match geometry {
        Geometry::Box { size } => Ok(half_norm(*size)),
        Geometry::Cylinder { radius, length } => Ok(radial_shape_bound(*radius, *length / 2.0)),
        Geometry::Capsule { radius, length } => {
            Ok(radial_shape_bound(*radius, *length / 2.0 + *radius))
        }
        Geometry::Sphere { radius } => Ok(*radius),
        Geometry::Mesh { .. } => Err(ModelError::FootprintMesh { link: link.clone() }),
    }
}

fn link_transform(structure: &Structure, target: &str) -> Result<Transform, ModelError> {
    fn find(
        structure: &Structure,
        current: &str,
        transform: Transform,
        target: &str,
        movable_joint: Option<&crate::model::identity::JointId>,
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

fn norm_xyz(value: [f64; 3]) -> f64 {
    (value
        .into_iter()
        .map(|coordinate| coordinate * coordinate)
        .sum::<f64>())
    .sqrt()
}

fn normalized(value: [f64; 3]) -> Option<[f64; 3]> {
    let length = norm_xyz(value);
    (length.is_finite() && length > 0.0).then(|| scale(value, 1.0 / length))
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left.into_iter().zip(right).map(|(a, b)| a * b).sum()
}

fn scale(value: [f64; 3], scalar: f64) -> [f64; 3] {
    value.map(|coordinate| coordinate * scalar)
}

fn subtract(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
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

    fn structure(links: Value, joints: Value) -> crate::model::structure::Structure {
        crate::model::compiler::structure(json!({
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
            Err(crate::model::ModelError::FootprintMesh { .. })
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
            Err(crate::model::ModelError::FootprintMovableJoint { .. })
        ));
    }

    #[test]
    fn component_rotation_exception_rejects_off_axis_collision() {
        let robot = structure(
            json!([
                link("base_footprint", json!([])),
                link("component_mount", json!([])),
            ]),
            json!([fixed_joint(
                "component_mount_joint",
                "base_footprint",
                "component_mount"
            )]),
        );
        let component = structure(
            json!([
                link("mount", json!([])),
                link("rotor", sphere_collision(0.1)),
            ]),
            json!([{
                "name": "motor_joint",
                "kind": "continuous",
                "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
                "parent": "mount",
                "child": "rotor",
                "axis": [0.0, 0.0, 1.0],
                "limit": { "lower": 0.0, "upper": 0.0, "effort": 0.0, "velocity": 1.0 }
            }]),
        );
        let component_type = crate::model::identity::ComponentTypeId::new("wheel")
            .expect("the component type id is normalized");
        let instance_id = crate::model::identity::ComponentInstanceId::new("wheel-1")
            .expect("the component instance id is normalized");
        let instance = crate::model::compiler::component_instance(
            component_type.clone(),
            crate::model::identity::LinkId::new("component_mount"),
            BTreeMap::new(),
            BTreeMap::new(),
            None,
        );
        let components = BTreeMap::from([(
            component_type,
            crate::model::compiler::component(BTreeMap::new(), component, None),
        )]);
        let instances = BTreeMap::from([(instance_id, instance)]);

        assert!(matches!(
            compile(&robot, &instances, &components),
            Err(crate::model::ModelError::FootprintMovableJoint { joint })
                if joint.as_str() == "motor_joint"
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
        let component_type = crate::model::identity::ComponentTypeId::new("tool")
            .expect("the test component type id is normalized");
        let instance_id = crate::model::identity::ComponentInstanceId::new("tool-1")
            .expect("the test component instance id is normalized");
        let instance = crate::model::compiler::component_instance(
            component_type.clone(),
            crate::model::identity::LinkId::new("arm"),
            BTreeMap::new(),
            BTreeMap::new(),
            None,
        );
        let components = BTreeMap::from([(
            component_type,
            crate::model::compiler::component(BTreeMap::new(), component, None),
        )]);
        let instances = BTreeMap::from([(instance_id, instance)]);

        assert!(matches!(
            compile(&robot, &instances, &components),
            Err(crate::model::ModelError::FootprintMovableJoint { .. })
        ));
    }
}
