//! Transform-tree math: composing the lookup response between any two known
//! frames through their lowest common ancestor, folding joint state into
//! child-to-parent transforms, and converting between the wire
//! `FrameTransform` type and `nalgebra` isometries.

use nalgebra::{Isometry3, Quaternion, Translation3, Unit, UnitQuaternion, Vector3};
use phoxal::api;
use phoxal::bus::RobotInstant;
use std::collections::{BTreeMap, BTreeSet};

use crate::config::{FrameJointType, JointMeta};
use crate::ring_buffer::RingBuffer;

pub(crate) fn lookup_transform(
    state: &crate::frame::FrameState,
    request: &api::frame::LookupRequest,
) -> Option<api::frame::FrameTransform> {
    let target = &request.target_frame_id;
    let source = &request.source_frame_id;

    if !known_frame(
        target,
        &state.static_transforms,
        &state.buffers,
        &state.parent_by_child,
    ) || !known_frame(
        source,
        &state.static_transforms,
        &state.buffers,
        &state.parent_by_child,
    ) {
        return None;
    }

    if target == source {
        return Some(transform_from_isometry(
            target.clone(),
            source.clone(),
            Isometry3::identity(),
            None,
        ));
    }

    let lca = common_ancestor(target, source, &state.parent_by_child)?;
    let (target_stamp, lca_to_target) = transform_from_ancestor_to_descendant(
        &lca,
        target,
        request.at,
        &state.static_transforms,
        &state.buffers,
        &state.parent_by_child,
    )?;
    let (source_stamp, lca_to_source) = transform_from_ancestor_to_descendant(
        &lca,
        source,
        request.at,
        &state.static_transforms,
        &state.buffers,
        &state.parent_by_child,
    )?;

    // The composed transform is only as fresh as its stalest edge; both edges
    // came from the same timeline (`nearest` rejects any other), so comparing
    // ticks is well-defined here.
    let stamp = target_stamp
        .into_iter()
        .chain(source_stamp)
        .max_by_key(|instant| instant.ticks());
    Some(transform_from_isometry(
        target.clone(),
        source.clone(),
        lca_to_target.inverse() * lca_to_source,
        stamp,
    ))
}

fn known_frame(
    frame_id: &str,
    statics: &BTreeMap<String, api::frame::FrameTransform>,
    dynamics: &BTreeMap<String, RingBuffer<Isometry3<f64>>>,
    parent_by_child: &BTreeMap<String, (String, JointMeta)>,
) -> bool {
    statics.contains_key(frame_id)
        || dynamics.contains_key(frame_id)
        || parent_by_child.contains_key(frame_id)
        || parent_by_child
            .values()
            .any(|(parent_frame_id, _)| parent_frame_id == frame_id)
}

fn common_ancestor(
    target: &str,
    source: &str,
    parent_by_child: &BTreeMap<String, (String, JointMeta)>,
) -> Option<String> {
    let target_ancestors = ancestors(target, parent_by_child);
    let mut current = source.to_string();
    loop {
        if target_ancestors.contains(&current) {
            return Some(current);
        }
        let (next, _) = parent_by_child.get(&current)?;
        current = next.clone();
    }
}

fn ancestors(
    frame_id: &str,
    parent_by_child: &BTreeMap<String, (String, JointMeta)>,
) -> BTreeSet<String> {
    let mut ancestors = BTreeSet::new();
    let mut current = frame_id.to_string();
    loop {
        ancestors.insert(current.clone());
        let Some((parent, _)) = parent_by_child.get(&current) else {
            return ancestors;
        };
        current = parent.clone();
    }
}

fn transform_from_ancestor_to_descendant(
    ancestor: &str,
    descendant: &str,
    at: Option<RobotInstant>,
    statics: &BTreeMap<String, api::frame::FrameTransform>,
    dynamics: &BTreeMap<String, RingBuffer<Isometry3<f64>>>,
    parent_by_child: &BTreeMap<String, (String, JointMeta)>,
) -> Option<(Option<RobotInstant>, Isometry3<f64>)> {
    let mut child_to_parent_edges = Vec::new();
    let mut current = descendant.to_string();

    while current != ancestor {
        let (parent, _) = parent_by_child.get(&current)?;
        child_to_parent_edges.push(edge_transform(&current, at, statics, dynamics)?);
        current = parent.clone();
    }

    let mut stamp = None;
    let mut transform = Isometry3::identity();
    for (edge_stamp, edge) in child_to_parent_edges.into_iter().rev() {
        // The composed transform is only as fresh as its stalest edge, so the
        // latest edge instant is the honest stamp. Edges on different
        // timelines cannot be composed at all, so `max_by_key` on ticks is
        // safe: `nearest` already rejected any foreign-timeline edge.
        stamp = stamp
            .into_iter()
            .chain(edge_stamp)
            .max_by_key(|instant| instant.ticks());
        transform *= edge;
    }
    Some((stamp, transform))
}

fn edge_transform(
    child_frame_id: &str,
    at: Option<RobotInstant>,
    statics: &BTreeMap<String, api::frame::FrameTransform>,
    dynamics: &BTreeMap<String, RingBuffer<Isometry3<f64>>>,
) -> Option<(Option<RobotInstant>, Isometry3<f64>)> {
    if let Some(transform) = statics.get(child_frame_id) {
        return Some((None, isometry_from_transform(transform)));
    }
    let buffer = dynamics.get(child_frame_id)?;
    let (stamp, transform) = match at {
        Some(at) => buffer.nearest(at)?,
        None => buffer.latest()?,
    };
    Some((Some(stamp), transform))
}

pub(crate) fn joint_transform(
    meta: &JointMeta,
    state: &api::joint::JointState,
) -> Option<Isometry3<f64>> {
    match meta.joint_type {
        FrameJointType::Fixed => Some(meta.origin),
        FrameJointType::Revolute | FrameJointType::Continuous => {
            let axis = joint_axis(meta)?;
            Some(
                meta.origin
                    * Isometry3::from_parts(
                        Translation3::identity(),
                        UnitQuaternion::from_axis_angle(&axis, state.position_rad),
                    ),
            )
        }
        FrameJointType::Prismatic => {
            let axis = joint_axis(meta)?.into_inner();
            Some(
                meta.origin
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
    }
}

fn joint_axis(meta: &JointMeta) -> Option<Unit<Vector3<f64>>> {
    Unit::try_new(
        Vector3::new(meta.axis_xyz[0], meta.axis_xyz[1], meta.axis_xyz[2]),
        f64::EPSILON,
    )
}

pub(crate) fn transform_from_isometry(
    parent_frame_id: String,
    child_frame_id: String,
    transform: Isometry3<f64>,
    stamp: Option<RobotInstant>,
) -> api::frame::FrameTransform {
    let q = transform.rotation.quaternion();
    api::frame::FrameTransform {
        parent_frame_id,
        child_frame_id,
        translation_m: [
            transform.translation.x,
            transform.translation.y,
            transform.translation.z,
        ],
        rotation_quat_xyzw: [q.i, q.j, q.k, q.w],
        stamp,
    }
}

fn isometry_from_transform(transform: &api::frame::FrameTransform) -> Isometry3<f64> {
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
