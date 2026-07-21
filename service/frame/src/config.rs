//! Robot-model-derived frame configuration: builds the static transform map,
//! the child-to-parent joint metadata, and the list of dynamically tracked
//! joints from the robot's link/joint structure.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use nalgebra::{Isometry3, Translation3, UnitQuaternion};
use phoxal::api;
use phoxal::model::structure::{Joint as UrdfJoint, JointType, Pose, Structure};
use phoxal::model::v0::Robot;

#[derive(Clone)]
pub(crate) struct FrameConfig {
    pub(crate) static_transforms: BTreeMap<String, api::frame::FrameTransform>,
    pub(crate) parent_by_child: BTreeMap<String, (String, JointMeta)>,
    pub(crate) dynamic_joints: Vec<DynamicJoint>,
}

impl FrameConfig {
    pub(crate) fn from_robot(robot: &Robot) -> Result<Self> {
        Self::from_structure(&robot.structure)
    }

    pub(crate) fn from_structure(structure: &Structure) -> Result<Self> {
        let mut static_transforms = BTreeMap::new();
        let mut parent_by_child = BTreeMap::new();
        let mut dynamic_joints = Vec::new();

        for joint in &structure.joints {
            let parent_frame_id = joint.parent.link.clone();
            let child_frame_id = joint.child.link.clone();
            let meta = JointMeta::from_joint(joint)?;

            parent_by_child.insert(
                child_frame_id.clone(),
                (parent_frame_id.clone(), meta.clone()),
            );
            if meta.joint_type == FrameJointType::Fixed {
                static_transforms.insert(
                    child_frame_id.clone(),
                    super::transform::transform_from_isometry(
                        parent_frame_id,
                        child_frame_id,
                        meta.origin,
                        None,
                    ),
                );
            } else {
                dynamic_joints.push(DynamicJoint {
                    joint_id: meta.joint_id.clone(),
                    child_frame_id,
                });
            }
        }

        Ok(Self {
            static_transforms,
            parent_by_child,
            dynamic_joints,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DynamicJoint {
    pub(crate) joint_id: String,
    pub(crate) child_frame_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct JointMeta {
    pub(crate) joint_id: String,
    pub(crate) joint_type: FrameJointType,
    pub(crate) origin: Isometry3<f64>,
    pub(crate) axis_xyz: [f64; 3],
}

impl JointMeta {
    fn from_joint(joint: &UrdfJoint) -> Result<Self> {
        Ok(Self {
            joint_id: joint.name.clone(),
            joint_type: FrameJointType::from_urdf(&joint.joint_type)?,
            origin: pose_to_isometry(&joint.origin),
            axis_xyz: [joint.axis.xyz[0], joint.axis.xyz[1], joint.axis.xyz[2]],
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameJointType {
    Fixed,
    Revolute,
    Continuous,
    Prismatic,
}

impl FrameJointType {
    fn from_urdf(joint_type: &JointType) -> Result<Self> {
        match joint_type {
            JointType::Fixed => Ok(Self::Fixed),
            JointType::Revolute => Ok(Self::Revolute),
            JointType::Continuous => Ok(Self::Continuous),
            JointType::Prismatic => Ok(Self::Prismatic),
            JointType::Floating | JointType::Planar | JointType::Spherical => {
                bail!("unsupported frame joint type {joint_type:?}")
            }
        }
    }
}

fn pose_to_isometry(pose: &Pose) -> Isometry3<f64> {
    Isometry3::from_parts(
        Translation3::new(pose.xyz[0], pose.xyz[1], pose.xyz[2]),
        UnitQuaternion::from_euler_angles(pose.rpy[0], pose.rpy[1], pose.rpy[2]),
    )
}
