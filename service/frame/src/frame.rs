//! `frame` - maintain the robot transform tree and serve frame lookups.
//!
//! A scheduled participant with a serialized query handler. It builds the link
//! tree from the robot model: fixed joints become static transforms, while
//! movable joints (revolute, continuous, prismatic) are tracked dynamically.
//! It subscribes to the per-joint `joint/<id>/state` topic for each movable joint
//! and folds each sample into the joint origin to produce a child-to-parent
//! transform, buffered in a time-windowed ring buffer per child frame.
//! Each step it publishes the latest combined tree on `frame/tree`, and emits the
//! static transforms once on `frame/static_transforms`.
//! The `frame/lookup` handler composes the transform between any two known frames through their
//! lowest common ancestor; a timestamped lookup resolves the latest dynamic
//! samples at or before the requested instant and returns no transform for
//! unknown frames or timestamps outside a dynamic frame's buffered window.
//! Floating, planar, and spherical joints are not supported and fail setup.

use anyhow::Result;
use nalgebra::Isometry3;
use phoxal::api;
use phoxal::model::identity::LinkId;
use phoxal::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

use crate::config::{DynamicJoint, FrameConfig, JointMeta};
use crate::ring_buffer::RingBuffer;
use crate::transform;

const BUFFER_WINDOW: std::time::Duration = std::time::Duration::from_nanos(5_000_000_000);
const BUFFER_MAX_ENTRIES: usize = 16_384;

/// One dynamically tracked joint: the subscription carrying its state, beside
/// the frame the folded transform is buffered under.
struct TrackedJoint {
    joint: DynamicJoint,
    states: StreamReceiver<api::joint::StateEndpoint>,
}

pub(crate) struct Api {
    joints: Vec<TrackedJoint>,
    tree: StatePublisher<api::frame::TreeEndpoint>,
    static_pub: StatePublisher<api::frame::StaticTransformsEndpoint>,
}

/// The transform tree: the fixed edges, the parent and joint each frame hangs
/// from, and the recent samples for every dynamically tracked edge.
pub(crate) struct FrameState {
    static_transforms: BTreeMap<LinkId, api::frame::FrameTransform>,
    parent_by_child: BTreeMap<LinkId, (LinkId, JointMeta)>,
    buffers: BTreeMap<LinkId, RingBuffer<Isometry3<f64>>>,
    published_static: bool,
}

#[phoxal::service(state = FrameState, api = Api)]
pub(crate) struct Frame;

impl Participant for Frame {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let config = FrameConfig::from_robot(ctx.robot()?)?;

        let mut joints = Vec::with_capacity(config.dynamic_joints.len());
        let mut buffers = BTreeMap::new();
        for dynamic in config.dynamic_joints {
            let states = ctx
                .stream_receiver(api::topic::client().joint(&dynamic.joint_id)?.state())
                .await?;
            buffers.insert(
                dynamic.child_frame_id.clone(),
                RingBuffer::new(BUFFER_WINDOW, BUFFER_MAX_ENTRIES),
            );
            joints.push(TrackedJoint {
                joint: dynamic,
                states,
            });
        }

        // Frame OWNS the `frame` node (tree, static transforms, and the
        // `frame/lookup` query it serves below) -> owner builder;
        // joint states are CONSUMED via the public builder.
        let tree = ctx.state_publisher(api::topic::owner().frame().tree())?;
        let static_pub = ctx.state_publisher(api::topic::owner().frame().static_transforms())?;
        ctx.query(api::topic::owner().frame().lookup(), Self::lookup)?;

        Ok((
            FrameState {
                static_transforms: config.static_transforms,
                parent_by_child: config.parent_by_child,
                buffers,
                published_static: false,
            },
            Api {
                joints,
                tree,
                static_pub,
            },
        ))
    }

    fn reset(&self, _ctx: ResetContext, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        for buffer in state.buffers.values_mut() {
            buffer.clear();
        }
        // Static transforms are immutable configuration and remain valid, but
        // republish them for the replacement execution.
        state.published_static = false;
        Ok(())
    }

    #[phoxal::step(hz = 50)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        for tracked in &api.joints {
            while let Some(observed) = tracked.states.try_recv()? {
                let Some(at) = observed.metadata.produced_exactly_at() else {
                    continue;
                };
                let Some((_, meta)) = state.parent_by_child.get(&tracked.joint.child_frame_id)
                else {
                    continue;
                };
                let Some(transform) = meta.transform(&observed.body) else {
                    continue;
                };
                state
                    .buffers
                    .entry(tracked.joint.child_frame_id.clone())
                    .or_insert_with(|| RingBuffer::new(BUFFER_WINDOW, BUFFER_MAX_ENTRIES))
                    .push(at, transform);
            }
        }

        if !state.published_static {
            api.static_pub.publish(
                &step.token,
                api::frame::StaticTransforms {
                    transforms: transform::sorted_transforms(
                        state.static_transforms.values().cloned(),
                    ),
                },
            )?;
            state.published_static = true;
        }

        api.tree.publish(
            &step.token,
            api::frame::Tree {
                transforms: state.tree_transforms(),
            },
        )?;
        Ok(())
    }
}

impl Frame {
    fn lookup(
        &self,
        _api: &Api,
        _query: QueryContext,
        request: api::frame::LookupRequest,
        state: &mut FrameState,
    ) -> QueryResult<api::frame::LookupResponse> {
        Ok(api::frame::LookupResponse {
            transform: state.lookup(&request),
        })
    }
}

impl FrameState {
    fn tree_transforms(&self) -> Vec<api::frame::FrameTransform> {
        let static_transforms = self.static_transforms.values().cloned();
        let dynamic_transforms = self.buffers.iter().filter_map(|(child_frame_id, buffer)| {
            let (stamp, transform) = buffer.latest()?;
            let (parent_frame_id, _) = self.parent_by_child.get(child_frame_id)?;
            Some(transform::transform_from_isometry(
                parent_frame_id,
                child_frame_id,
                transform,
                Some(stamp),
            ))
        });
        transform::sorted_transforms(static_transforms.chain(dynamic_transforms))
    }

    /// The transform between the two requested frames, composed through their
    /// lowest common ancestor.
    ///
    /// Absent when either frame is unknown to this tree, when they share no
    /// ancestor, or when a dynamic edge on the path has no sample near the
    /// requested instant.
    fn lookup(&self, request: &api::frame::LookupRequest) -> Option<api::frame::FrameTransform> {
        let target = self.known_frame(&request.target_frame_id)?;
        let source = self.known_frame(&request.source_frame_id)?;

        if target == source {
            return Some(transform::transform_from_isometry(
                target,
                source,
                Isometry3::identity(),
                None,
            ));
        }

        let ancestor = self.common_ancestor(target, source)?;
        let (target_stamp, ancestor_to_target) =
            self.compose_from_ancestor(ancestor, target, request.at)?;
        let (source_stamp, ancestor_to_source) =
            self.compose_from_ancestor(ancestor, source, request.at)?;

        // The composed transform is only as fresh as its stalest edge; both edges
        // came from the same timeline (`at_or_before` rejects any other), so comparing
        // ticks is well-defined here.
        let stamp = target_stamp
            .into_iter()
            .chain(source_stamp)
            .min_by_key(|instant| instant.ticks());
        Some(transform::transform_from_isometry(
            target,
            source,
            ancestor_to_target.inverse() * ancestor_to_source,
            stamp,
        ))
    }

    /// The tree's own identity for `frame_id`, if it knows the frame at all.
    ///
    /// Returning the identity rather than a `bool` is what lets the composed
    /// response name frames the tree owns instead of echoing request text back.
    /// The root frame is never a key, only a parent, so the parent scan is not
    /// redundant with the keyed lookups.
    fn known_frame(&self, frame_id: &str) -> Option<&LinkId> {
        self.static_transforms
            .get_key_value(frame_id)
            .map(|(id, _)| id)
            .or_else(|| self.buffers.get_key_value(frame_id).map(|(id, _)| id))
            .or_else(|| {
                self.parent_by_child
                    .get_key_value(frame_id)
                    .map(|(id, _)| id)
            })
            .or_else(|| {
                self.parent_by_child
                    .values()
                    .map(|(parent_frame_id, _)| parent_frame_id)
                    .find(|parent_frame_id| parent_frame_id.as_str() == frame_id)
            })
    }

    /// The lowest frame that is an ancestor of both, or `None` when the two
    /// frames sit in disconnected parts of the tree.
    fn common_ancestor<'a>(&'a self, target: &'a LinkId, source: &'a LinkId) -> Option<&'a LinkId> {
        let target_ancestors = self.ancestors(target);
        let mut seen = BTreeSet::new();
        let mut current = source;
        loop {
            // Revisiting a frame means the parent chain closed on itself, so
            // there is no ancestor to find and the walk would not terminate.
            if !seen.insert(current) {
                return None;
            }
            if target_ancestors.contains(current) {
                return Some(current);
            }
            let (parent, _) = self.parent_by_child.get(current)?;
            current = parent;
        }
    }

    fn ancestors<'a>(&'a self, frame_id: &'a LinkId) -> BTreeSet<&'a LinkId> {
        let mut ancestors = BTreeSet::new();
        let mut current = frame_id;
        loop {
            if !ancestors.insert(current) {
                return ancestors;
            }
            let Some((parent, _)) = self.parent_by_child.get(current) else {
                return ancestors;
            };
            current = parent;
        }
    }

    /// Compose every edge from `ancestor` down to `descendant`, with the stamp
    /// of the stalest edge on the path.
    fn compose_from_ancestor<'a>(
        &'a self,
        ancestor: &LinkId,
        descendant: &'a LinkId,
        at: Option<RobotInstant>,
    ) -> Option<(Option<RobotInstant>, Isometry3<f64>)> {
        let mut child_to_parent_edges = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = descendant;

        while current != ancestor {
            if !seen.insert(current) {
                return None;
            }
            let (parent, _) = self.parent_by_child.get(current)?;
            child_to_parent_edges.push(self.edge_transform(current, at)?);
            current = parent;
        }

        let mut stamp = None;
        let mut transform = Isometry3::identity();
        for (edge_stamp, edge) in child_to_parent_edges.into_iter().rev() {
            // The composed transform is only as fresh as its stalest edge, so the
            // oldest edge instant is the honest stamp. Edges on different
            // timelines cannot be composed at all, so `min_by_key` on ticks is
            // safe: `at_or_before` already rejected any foreign-timeline edge.
            stamp = stamp
                .into_iter()
                .chain(edge_stamp)
                .min_by_key(|instant| instant.ticks());
            transform *= edge;
        }
        Some((stamp, transform))
    }

    /// The single child-to-parent edge above `child_frame_id`. A static edge
    /// carries no stamp: it is configuration, not observation.
    fn edge_transform(
        &self,
        child_frame_id: &LinkId,
        at: Option<RobotInstant>,
    ) -> Option<(Option<RobotInstant>, Isometry3<f64>)> {
        if let Some(edge) = self.static_transforms.get(child_frame_id) {
            return Some((None, transform::isometry_from_transform(edge)));
        }
        let buffer = self.buffers.get(child_frame_id)?;
        let (stamp, transform) = match at {
            Some(at) => buffer.at_or_before(at)?,
            None => buffer.latest()?,
        };
        Some((Some(stamp), transform))
    }
}

#[cfg(test)]
mod tests {
    use nalgebra::{Quaternion, UnitQuaternion};
    use phoxal::model::structure::JointKind;
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};

    use super::*;

    /// One fixed timeline for the frame tests, so the buffered instants read
    /// like the tick counters these cases care about.
    fn line() -> phoxal::bus::TimelineId {
        phoxal::bus::TimelineId::from_raw(1).expect("test timeline must be nonzero")
    }

    fn at(ticks: u64) -> RobotInstant {
        RobotInstant::new(line(), ticks)
    }

    const EPSILON: f64 = 1e-9;

    fn config_with_joint(
        name: &str,
        kind: JointKind,
        child: &str,
        rpy: [f64; 3],
        axis: [f64; 3],
    ) -> FrameConfig {
        let child = LinkId::new(child);
        let parent = LinkId::new("base_link");
        let origin = Isometry3::from_parts(
            nalgebra::Translation3::identity(),
            UnitQuaternion::from_euler_angles(rpy[0], rpy[1], rpy[2]),
        );
        let meta = JointMeta {
            joint_id: phoxal::model::identity::JointId::new(name),
            kind,
            origin,
            axis_xyz: axis,
        };
        let static_transforms = if kind == JointKind::Fixed {
            BTreeMap::from([(
                child.clone(),
                transform::transform_from_isometry(&parent, &child, origin, None),
            )])
        } else {
            BTreeMap::new()
        };
        let dynamic_joints = if kind == JointKind::Fixed {
            Vec::new()
        } else {
            vec![DynamicJoint {
                joint_id: meta.joint_id.clone(),
                child_frame_id: child.clone(),
            }]
        };
        FrameConfig {
            static_transforms,
            parent_by_child: BTreeMap::from([(child, (parent, meta))]),
            dynamic_joints,
        }
    }

    fn state_from_config(
        config: &FrameConfig,
        buffers: BTreeMap<LinkId, RingBuffer<Isometry3<f64>>>,
    ) -> FrameState {
        FrameState {
            static_transforms: config.static_transforms.clone(),
            parent_by_child: config.parent_by_child.clone(),
            buffers,
            published_static: false,
        }
    }

    fn single_dynamic_config() -> FrameConfig {
        config_with_joint(
            "wheel_joint",
            JointKind::Continuous,
            "wheel_link",
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
        )
    }

    /// A wheel-link buffer holding `positions_rad` at successive tick stamps.
    fn wheel_buffer(config: &FrameConfig, samples: &[(u64, f64)]) -> RingBuffer<Isometry3<f64>> {
        let (_, meta) = config
            .parent_by_child
            .get("wheel_link")
            .expect("wheel metadata");
        let mut buffer = RingBuffer::new(BUFFER_WINDOW, BUFFER_MAX_ENTRIES);
        for (ticks, position_rad) in samples {
            assert!(
                buffer.push(
                    at(*ticks),
                    meta.transform(&joint_state(*position_rad))
                        .expect("joint transform"),
                )
            );
        }
        buffer
    }

    fn wheel_state(config: &FrameConfig, samples: &[(u64, f64)]) -> FrameState {
        state_from_config(
            config,
            BTreeMap::from([(LinkId::new("wheel_link"), wheel_buffer(config, samples))]),
        )
    }

    fn request(source: &str, at: Option<RobotInstant>) -> api::frame::LookupRequest {
        api::frame::LookupRequest {
            target_frame_id: "base_link".to_string(),
            source_frame_id: source.to_string(),
            at,
        }
    }

    #[test]
    fn static_chain_lookup_composes_yaw() {
        let config = config_with_joint(
            "arm_mount",
            JointKind::Fixed,
            "arm_link",
            [0.0, 0.0, FRAC_PI_2],
            [0.0, 0.0, 0.0],
        );
        let state = state_from_config(&config, BTreeMap::new());

        let transform = state
            .lookup(&request("arm_link", Some(at(0))))
            .expect("static lookup should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp, None);
    }

    #[test]
    fn dynamic_joint_lookup_uses_latest_sample_at_or_before_request() {
        let config = single_dynamic_config();
        let state = wheel_state(&config, &[(100, FRAC_PI_4), (200, FRAC_PI_2)]);

        let transform = state
            .lookup(&request("wheel_link", Some(at(175))))
            .expect("dynamic lookup should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_4);
        assert_eq!(transform.stamp, Some(at(100)));
    }

    #[test]
    fn out_of_order_dynamic_samples_do_not_change_the_greatest_time_latest() {
        let config = single_dynamic_config();
        let state = wheel_state(&config, &[(200, FRAC_PI_2), (100, FRAC_PI_4)]);

        let transform = state
            .lookup(&request("wheel_link", None))
            .expect("latest lookup should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp, Some(at(200)));
    }

    #[test]
    fn composed_lookup_stamp_is_the_stalest_dynamic_edge() {
        let base = LinkId::new("base_link");
        let middle = LinkId::new("middle_link");
        let tip = LinkId::new("tool_link");
        let middle_meta = JointMeta {
            joint_id: phoxal::model::identity::JointId::new("middle_joint"),
            kind: JointKind::Continuous,
            origin: Isometry3::identity(),
            axis_xyz: [0.0, 0.0, 1.0],
        };
        let tip_meta = JointMeta {
            joint_id: phoxal::model::identity::JointId::new("tool_joint"),
            kind: JointKind::Continuous,
            origin: Isometry3::identity(),
            axis_xyz: [0.0, 0.0, 1.0],
        };

        let mut middle_buffer = RingBuffer::new(BUFFER_WINDOW, BUFFER_MAX_ENTRIES);
        assert!(
            middle_buffer.push(
                at(100),
                middle_meta
                    .transform(&joint_state(0.0))
                    .expect("middle joint transform"),
            )
        );
        let mut tip_buffer = RingBuffer::new(BUFFER_WINDOW, BUFFER_MAX_ENTRIES);
        assert!(
            tip_buffer.push(
                at(200),
                tip_meta
                    .transform(&joint_state(0.0))
                    .expect("tip joint transform"),
            )
        );

        let state = FrameState {
            static_transforms: BTreeMap::new(),
            parent_by_child: BTreeMap::from([
                (middle.clone(), (base, middle_meta)),
                (tip.clone(), (middle, tip_meta)),
            ]),
            buffers: BTreeMap::from([
                (LinkId::new("middle_link"), middle_buffer),
                (tip, tip_buffer),
            ]),
            published_static: false,
        };

        let transform = state
            .lookup(&request("tool_link", Some(at(200))))
            .expect("a dynamic chain lookup should resolve");
        assert_eq!(
            transform.stamp,
            Some(at(100)),
            "a composed transform is only as fresh as its oldest dynamic edge"
        );
    }

    #[test]
    fn lookup_returns_none_for_unknown_or_out_of_range_frames() {
        let config = single_dynamic_config();
        let state = wheel_state(&config, &[(100, 0.0)]);

        assert!(state.lookup(&request("missing", Some(at(100)))).is_none());
        assert!(
            state
                .lookup(&request(
                    "wheel_link",
                    Some(
                        at(100).saturating_add(BUFFER_WINDOW + std::time::Duration::from_nanos(1))
                    )
                ))
                .is_none()
        );
    }

    #[test]
    fn lookup_at_or_after_newest_sample_uses_latest_within_tolerance() {
        let config = single_dynamic_config();
        let state = wheel_state(&config, &[(100, FRAC_PI_4), (200, FRAC_PI_2)]);

        let transform = state
            .lookup(&request("wheel_link", Some(at(250))))
            .expect("latest within tolerance should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp, Some(at(200)));
    }

    #[test]
    fn latest_lookup_uses_newest_dynamic_sample() {
        let config = single_dynamic_config();
        let state = wheel_state(&config, &[(100, FRAC_PI_4), (200, FRAC_PI_2)]);

        let transform = state
            .lookup(&request("wheel_link", None))
            .expect("latest lookup should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp, Some(at(200)));
    }

    fn joint_state(position_rad: f64) -> api::joint::JointState {
        api::joint::JointState {
            position_rad,
            velocity_radps: 0.0,
            effort_nm: None,
        }
    }

    fn assert_yaw(rotation_xyzw: [f64; 4], expected_yaw: f64) {
        let rotation = UnitQuaternion::from_quaternion(Quaternion::new(
            rotation_xyzw[3],
            rotation_xyzw[0],
            rotation_xyzw[1],
            rotation_xyzw[2],
        ));
        let (_, _, yaw) = rotation.euler_angles();
        assert!(
            (yaw - expected_yaw).abs() <= EPSILON,
            "expected {expected_yaw}, got {yaw}"
        );
    }
}
