//! Robot-model-derived frame configuration: the static transform map, the
//! child-to-parent joint metadata, and the list of dynamically tracked joints,
//! all built from the robot's flattened link and joint structure.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use nalgebra::{Isometry3, Translation3, Unit, UnitQuaternion, Vector3};
use phoxal::api;
use phoxal::model::Robot;
use phoxal::model::identity::{ComponentInstanceId, JointId, LinkId};
use phoxal::model::structure::{Joint, JointKind, Pose, Structure};

use crate::transform;

/// The joint this service synthesizes to attach a mounted component's root link
/// to the robot link it is mounted on. The robot structure carries no joint for
/// that attachment, because the mount is declared on the instance instead.
const MOUNT_ATTACH_JOINT: &str = "mount_attach";

#[derive(Clone)]
pub(crate) struct FrameConfig {
    pub(crate) static_transforms: BTreeMap<LinkId, api::frame::FrameTransform>,
    pub(crate) parent_by_child: BTreeMap<LinkId, (LinkId, JointMeta)>,
    pub(crate) dynamic_joints: Vec<DynamicJoint>,
}

impl FrameConfig {
    pub(crate) fn from_robot(robot: &Robot) -> Result<Self> {
        let mut config = Self::from_structure(robot.structure())?;
        for mounted in robot.components() {
            let id = mounted.id();
            let structure = mounted.component_type().structure();
            config.add_joint(
                mounted.instance().mount_link().clone(),
                structure.root_link().namespaced(id),
                JointMeta {
                    joint_id: JointId::new(MOUNT_ATTACH_JOINT).namespaced(id),
                    kind: JointKind::Fixed,
                    origin: Isometry3::identity(),
                    axis_xyz: [0.0; 3],
                },
            )?;
            for joint in structure.joints() {
                config.add_joint(
                    joint.parent().namespaced(id),
                    joint.child().namespaced(id),
                    JointMeta::from_joint(joint)?.namespaced(id),
                )?;
            }
        }
        Ok(config)
    }

    pub(crate) fn from_structure(structure: &Structure) -> Result<Self> {
        let mut config = Self {
            static_transforms: BTreeMap::new(),
            parent_by_child: BTreeMap::new(),
            dynamic_joints: Vec::new(),
        };
        for joint in structure.joints() {
            config.add_joint(
                joint.parent().clone(),
                joint.child().clone(),
                JointMeta::from_joint(joint)?,
            )?;
        }
        Ok(config)
    }

    /// Record one parent-to-child edge.
    ///
    /// A frame with two parents, or a joint id used twice, would make the tree
    /// walk ambiguous, so both are refused here rather than resolved by
    /// iteration order later.
    fn add_joint(
        &mut self,
        parent_frame_id: LinkId,
        child_frame_id: LinkId,
        meta: JointMeta,
    ) -> Result<()> {
        if self.parent_by_child.contains_key(&child_frame_id) {
            bail!("frame '{child_frame_id}' has more than one parent");
        }
        if self
            .parent_by_child
            .values()
            .any(|(_, existing)| existing.joint_id == meta.joint_id)
        {
            bail!("frame joint '{}' is not unique", meta.joint_id);
        }
        if meta.kind == JointKind::Fixed {
            self.static_transforms.insert(
                child_frame_id.clone(),
                transform::transform_from_isometry(
                    &parent_frame_id,
                    &child_frame_id,
                    meta.origin,
                    None,
                ),
            );
        } else {
            self.dynamic_joints.push(DynamicJoint {
                joint_id: meta.joint_id.clone(),
                child_frame_id: child_frame_id.clone(),
            });
        }
        self.parent_by_child
            .insert(child_frame_id, (parent_frame_id, meta));
        Ok(())
    }
}

/// A joint whose transform is republished from live joint state rather than
/// fixed by the structure.
#[derive(Clone, Debug)]
pub(crate) struct DynamicJoint {
    pub(crate) joint_id: JointId,
    pub(crate) child_frame_id: LinkId,
}

/// What the transform tree needs to know about one joint to fold joint state
/// into a child-to-parent transform.
#[derive(Clone, Debug)]
pub(crate) struct JointMeta {
    pub(crate) joint_id: JointId,
    pub(crate) kind: JointKind,
    pub(crate) origin: Isometry3<f64>,
    pub(crate) axis_xyz: [f64; 3],
}

impl JointMeta {
    fn from_joint(joint: &Joint) -> Result<Self> {
        match joint.kind() {
            JointKind::Fixed
            | JointKind::Revolute
            | JointKind::Continuous
            | JointKind::Prismatic => {}
            kind @ (JointKind::Floating | JointKind::Planar | JointKind::Spherical) => {
                bail!("unsupported frame joint type {kind:?}")
            }
        }
        Ok(Self {
            joint_id: joint.name().clone(),
            kind: joint.kind(),
            origin: pose_to_isometry(joint.origin()),
            axis_xyz: joint.axis(),
        })
    }

    /// The same joint as the robot's flattened structure names it, under the
    /// component instance that mounts it.
    fn namespaced(mut self, component_id: &ComponentInstanceId) -> Self {
        self.joint_id = self.joint_id.namespaced(component_id);
        self
    }

    /// The child-to-parent transform this joint produces at `state`.
    ///
    /// Absent when a movable joint's axis is degenerate, which leaves its
    /// direction of motion - and therefore the transform - undefined.
    pub(crate) fn transform(&self, state: &api::joint::JointState) -> Option<Isometry3<f64>> {
        match self.kind {
            JointKind::Fixed => Some(self.origin),
            JointKind::Revolute | JointKind::Continuous => {
                let axis = self.axis()?;
                Some(
                    self.origin
                        * Isometry3::from_parts(
                            Translation3::identity(),
                            UnitQuaternion::from_axis_angle(&axis, state.position_rad),
                        ),
                )
            }
            JointKind::Prismatic => {
                let axis = self.axis()?.into_inner();
                Some(
                    self.origin
                        * Isometry3::from_parts(
                            Translation3::new(
                                axis.x * state.position_rad,
                                axis.y * state.position_rad,
                                axis.z * state.position_rad,
                            ),
                            UnitQuaternion::identity(),
                        ),
                )
            }
            // Refused by `from_joint`, so no configured joint is ever one of
            // these; the arm keeps the match exhaustive over the real kinds.
            JointKind::Floating | JointKind::Planar | JointKind::Spherical => None,
        }
    }

    fn axis(&self) -> Option<Unit<Vector3<f64>>> {
        Unit::try_new(
            Vector3::new(self.axis_xyz[0], self.axis_xyz[1], self.axis_xyz[2]),
            f64::EPSILON,
        )
    }
}

fn pose_to_isometry(pose: Pose) -> Isometry3<f64> {
    let xyz = pose.xyz();
    let rpy = pose.rpy();
    Isometry3::from_parts(
        Translation3::new(xyz[0], xyz[1], xyz[2]),
        UnitQuaternion::from_euler_angles(rpy[0], rpy[1], rpy[2]),
    )
}

#[cfg(test)]
mod tests {
    use super::FrameConfig;
    use phoxal::model::RobotBuilder;
    use phoxal::model::builder::Joint;
    use phoxal::model::identity::CapabilityRef;
    use phoxal::model::structure::JointKind;

    #[test]
    fn composes_component_frames_and_namespaced_dynamic_joints() {
        // A wheel whose rotor turns on a continuous joint, and a camera whose
        // lens is fixed to its body: the two cases the composition splits on.
        let robot = RobotBuilder::new("rover")
            .component_type("drive_motor", |motor| {
                motor
                    .joint(Joint {
                        name: "motor_joint",
                        kind: JointKind::Continuous,
                        parent: "mount",
                        child: "rotor_link",
                        ..Joint::default()
                    })
                    .motor("motor", "motor_joint")
            })
            .component_type("rgbd", |camera| camera.camera("rgb", "rgb_link"))
            .component_with("front_left_drive", "drive_motor", |mounted| {
                mounted.mounted_on("front_left_wheel_mount")
            })
            .component("front_camera", "rgbd")
            .build()
            .expect("a valid robot");

        let config = FrameConfig::from_robot(&robot).unwrap();

        // The mount attachment the robot structure carries no joint for.
        assert_eq!(
            config
                .parent_by_child
                .get("front_left_drive__mount")
                .map(|(parent, _)| parent.as_str()),
            Some("front_left_wheel_mount")
        );
        assert!(config.dynamic_joints.iter().any(|joint| {
            joint.joint_id == "front_left_drive__motor_joint"
                && joint.child_frame_id == "front_left_drive__rotor_link"
        }));
        let sensor_frame = robot
            .link_target_frame(&"front_camera.rgb".parse::<CapabilityRef>().unwrap())
            .unwrap();
        assert_eq!(sensor_frame, "front_camera__rgb_link");
        assert!(config.parent_by_child.contains_key(&sensor_frame));
    }
}
