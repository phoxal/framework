//! Robot-model-derived frame configuration: builds the static transform map,
//! the child-to-parent joint metadata, and the list of dynamically tracked
//! joints from the robot's link/joint structure.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use nalgebra::{Isometry3, Translation3, UnitQuaternion};
use phoxal::api;
use phoxal::model::Robot;
use phoxal::model::component::capability::namespaced_structure_id;
use phoxal::model::structure::{Joint, JointKind, Pose, Structure};

#[derive(Clone)]
pub(crate) struct FrameConfig {
    pub(crate) static_transforms: BTreeMap<String, api::frame::FrameTransform>,
    pub(crate) parent_by_child: BTreeMap<String, (String, JointMeta)>,
    pub(crate) dynamic_joints: Vec<DynamicJoint>,
}

impl FrameConfig {
    pub(crate) fn from_robot(robot: &Robot) -> Result<Self> {
        let mut config = Self::from_structure(robot.structure())?;
        for instance in robot.components() {
            let component = robot.component_for_instance(instance.id())?;
            let component_root =
                namespaced_structure_id(instance.id(), component.structure().root_link().name());
            config.add_joint(
                instance.mount_link().to_string(),
                component_root,
                JointMeta {
                    joint_id: namespaced_structure_id(instance.id(), "mount_attach"),
                    joint_type: FrameJointType::Fixed,
                    origin: Isometry3::identity(),
                    axis_xyz: [0.0; 3],
                },
            )?;
            for joint in component.structure().joints() {
                config.add_joint(
                    namespaced_structure_id(instance.id(), joint.parent()),
                    namespaced_structure_id(instance.id(), joint.child()),
                    JointMeta::from_namespaced_joint(joint, instance.id())?,
                )?;
            }
        }
        Ok(config)
    }

    pub(crate) fn from_structure(structure: &Structure) -> Result<Self> {
        let mut static_transforms = BTreeMap::new();
        let mut parent_by_child = BTreeMap::new();
        let mut dynamic_joints = Vec::new();

        for joint in structure.joints() {
            let parent_frame_id = joint.parent().to_string();
            let child_frame_id = joint.child().to_string();
            let meta = JointMeta::from_joint(joint)?;

            add_joint(
                &mut static_transforms,
                &mut parent_by_child,
                &mut dynamic_joints,
                parent_frame_id,
                child_frame_id,
                meta,
            )?;
        }

        Ok(Self {
            static_transforms,
            parent_by_child,
            dynamic_joints,
        })
    }

    fn add_joint(
        &mut self,
        parent_frame_id: String,
        child_frame_id: String,
        meta: JointMeta,
    ) -> Result<()> {
        add_joint(
            &mut self.static_transforms,
            &mut self.parent_by_child,
            &mut self.dynamic_joints,
            parent_frame_id,
            child_frame_id,
            meta,
        )
    }
}

fn add_joint(
    static_transforms: &mut BTreeMap<String, api::frame::FrameTransform>,
    parent_by_child: &mut BTreeMap<String, (String, JointMeta)>,
    dynamic_joints: &mut Vec<DynamicJoint>,
    parent_frame_id: String,
    child_frame_id: String,
    meta: JointMeta,
) -> Result<()> {
    if parent_by_child.contains_key(&child_frame_id) {
        bail!("frame '{child_frame_id}' has more than one parent");
    }
    if parent_by_child
        .values()
        .any(|(_, existing)| existing.joint_id == meta.joint_id)
    {
        bail!("frame joint '{}' is not unique", meta.joint_id);
    }
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
    Ok(())
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
    fn from_joint(joint: &Joint) -> Result<Self> {
        Ok(Self {
            joint_id: joint.name().to_string(),
            joint_type: FrameJointType::from_canonical(joint.kind())?,
            origin: pose_to_isometry(joint.origin()),
            axis_xyz: joint.axis(),
        })
    }

    fn from_namespaced_joint(joint: &Joint, component_id: &str) -> Result<Self> {
        let mut meta = Self::from_joint(joint)?;
        meta.joint_id = namespaced_structure_id(component_id, joint.name());
        Ok(meta)
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
    fn from_canonical(joint_type: JointKind) -> Result<Self> {
        match joint_type {
            JointKind::Fixed => Ok(Self::Fixed),
            JointKind::Revolute => Ok(Self::Revolute),
            JointKind::Continuous => Ok(Self::Continuous),
            JointKind::Prismatic => Ok(Self::Prismatic),
            JointKind::Floating | JointKind::Planar | JointKind::Spherical => {
                bail!("unsupported frame joint type {joint_type:?}")
            }
        }
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
    use phoxal::model::Robot;
    use phoxal::model::component::CapabilityRef;

    const GOLDEN: &[u8] =
        include_bytes!("../../../phoxal-model/tests/golden/rgbd-imu-diff-drive.robot.json");

    #[test]
    fn composes_component_frames_and_namespaced_dynamic_joints() {
        let robot = Robot::decode(GOLDEN).unwrap();
        let config = FrameConfig::from_robot(&robot).unwrap();

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
            .link_target_frame(&CapabilityRef::new("front_camera", "rgb"))
            .unwrap();
        assert_eq!(sensor_frame, "front_camera__rgb_link");
        assert!(config.parent_by_child.contains_key(&sensor_frame));
    }
}
