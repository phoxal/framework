//! Conversions between the wire `api::frame::FrameTransform` and the
//! `nalgebra` isometry the tree composes in, plus the deterministic order every
//! published transform list carries.
//!
//! Neither side of the conversion is a type this crate owns, so the pair cannot
//! be written as `From` impls here.

use nalgebra::{Isometry3, Quaternion, Translation3, UnitQuaternion};
use phoxal::api;
use phoxal::bus::RobotInstant;
use phoxal::model::identity::LinkId;

pub(crate) fn transform_from_isometry(
    parent_frame_id: &LinkId,
    child_frame_id: &LinkId,
    transform: Isometry3<f64>,
    stamp: Option<RobotInstant>,
) -> api::frame::FrameTransform {
    let q = transform.rotation.quaternion();
    api::frame::FrameTransform {
        parent_frame_id: parent_frame_id.to_string(),
        child_frame_id: child_frame_id.to_string(),
        translation_m: [
            transform.translation.x,
            transform.translation.y,
            transform.translation.z,
        ],
        rotation_quat_xyzw: [q.i, q.j, q.k, q.w],
        stamp,
    }
}

pub(crate) fn isometry_from_transform(transform: &api::frame::FrameTransform) -> Isometry3<f64> {
    Isometry3::from_parts(
        Translation3::new(
            transform.translation_m[0],
            transform.translation_m[1],
            transform.translation_m[2],
        ),
        UnitQuaternion::from_quaternion(Quaternion::new(
            transform.rotation_quat_xyzw[3],
            transform.rotation_quat_xyzw[0],
            transform.rotation_quat_xyzw[1],
            transform.rotation_quat_xyzw[2],
        )),
    )
}

/// Order a published transform list by child then parent frame, so two runs
/// over the same tree publish the same sequence.
pub(crate) fn sorted_transforms(
    transforms: impl IntoIterator<Item = api::frame::FrameTransform>,
) -> Vec<api::frame::FrameTransform> {
    let mut transforms = transforms.into_iter().collect::<Vec<_>>();
    transforms.sort_by(|left, right| {
        left.child_frame_id
            .cmp(&right.child_frame_id)
            .then_with(|| left.parent_frame_id.cmp(&right.parent_frame_id))
    });
    transforms
}
