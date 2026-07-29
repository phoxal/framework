//! `frame` - maintain the robot transform tree and serve frame lookups.
//!
//! A scheduled participant with a serialized query handler. It builds the link
//! tree from the robot model (D33): fixed joints become static transforms, while
//! movable joints (revolute, continuous, prismatic) are tracked dynamically.
//! It subscribes to the per-joint `joint/<id>/state` topic for each movable joint
//! and folds each sample into the joint origin to produce a child-to-parent
//! transform, buffered in a time-windowed ring buffer per child frame.
//! Each step it publishes the latest combined tree on `frame/tree`, and emits the
//! static transforms once on `frame/static_transforms`.
//! The `frame/lookup` handler composes the transform between any two known frames through their
//! lowest common ancestor; it returns no transform for unknown frames or for
//! timestamps outside a dynamic frame's buffered window.
//! Floating, planar, and spherical joints are not supported and fail setup.

use anyhow::Result;
use nalgebra::Isometry3;
use phoxal::api;
use phoxal::prelude::*;
use std::collections::BTreeMap;

use crate::config::{DynamicJoint, FrameConfig, JointMeta};
use crate::ring_buffer::RingBuffer;
use crate::transform;
use crate::transform::{joint_transform, lookup_transform, sorted_transforms};

const BUFFER_WINDOW: std::time::Duration = std::time::Duration::from_nanos(5_000_000_000);
const BUFFER_MAX_ENTRIES: usize = 16_384;

pub struct Api {
    joints: Vec<Subscriber<api::joint::JointState>>,
    tree: StatePublisher<api::frame::Tree>,
    static_pub: StatePublisher<api::frame::StaticTransforms>,
}

pub struct FrameState {
    pub(crate) static_transforms: BTreeMap<String, api::frame::FrameTransform>,
    pub(crate) parent_by_child: BTreeMap<String, (String, JointMeta)>,
    dynamic_joints: Vec<DynamicJoint>,
    pub(crate) buffers: BTreeMap<String, RingBuffer<Isometry3<f64>>>,
    published_static: bool,
}

#[phoxal::service(state = FrameState, api = Api)]
pub struct Frame;

impl Participant for Frame {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let config = FrameConfig::from_robot(ctx.robot()?)?;

        let mut joints = Vec::with_capacity(config.dynamic_joints.len());
        let mut buffers = BTreeMap::new();
        for dynamic in &config.dynamic_joints {
            joints.push(
                ctx.subscriber(api::topic::client().joint(&dynamic.joint_id).state(), 32)
                    .await?,
            );
            buffers.insert(
                dynamic.child_frame_id.clone(),
                RingBuffer::new(BUFFER_WINDOW, BUFFER_MAX_ENTRIES),
            );
        }

        // Frame OWNS the `frame` node (tree, static transforms, and the
        // `frame/lookup` query it serves below) -> owner builder;
        // joint states are CONSUMED via the public builder.
        let tree = ctx
            .state_publisher(api::topic::owner().frame().tree())
            .await?;
        let static_pub = ctx
            .state_publisher(api::topic::owner().frame().static_transforms())
            .await?;
        ctx.query(api::topic::owner().frame().lookup(), Self::lookup)
            .await?;

        Ok((
            FrameState {
                static_transforms: config.static_transforms,
                parent_by_child: config.parent_by_child,
                dynamic_joints: config.dynamic_joints,
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

    async fn reset(
        &self,
        _ctx: ResetContext,
        _api: &Self::Api,
        state: &mut Self::State,
    ) -> Result<()> {
        for buffer in state.buffers.values_mut() {
            buffer.clear();
        }
        // Static transforms are immutable configuration and remain valid, but
        // republish them for the replacement execution.
        state.published_static = false;
        Ok(())
    }

    #[phoxal::step(hz = 50)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        for (subscriber, dynamic) in api.joints.iter().zip(&state.dynamic_joints) {
            while let Some(observed) = subscriber.try_recv() {
                let Some(at) = observed.metadata.produced_exactly_at() else {
                    continue;
                };
                let Some((_, meta)) = state.parent_by_child.get(&dynamic.child_frame_id) else {
                    continue;
                };
                let Some(transform) = joint_transform(meta, &observed.body) else {
                    continue;
                };
                state
                    .buffers
                    .entry(dynamic.child_frame_id.clone())
                    .or_insert_with(|| RingBuffer::new(BUFFER_WINDOW, BUFFER_MAX_ENTRIES))
                    .push(at, transform);
            }
        }

        if !state.published_static {
            api.static_pub.publish(
                step.token(),
                api::frame::StaticTransforms {
                    transforms: sorted_transforms(state.static_transforms.values().cloned()),
                },
            )?;
            state.published_static = true;
        }

        api.tree.publish(
            step.token(),
            api::frame::Tree {
                transforms: state.tree_transforms(),
            },
        )?;
        Ok(())
    }
}

impl Frame {
    async fn lookup(
        &self,
        _api: &Api,
        request: api::frame::LookupRequest,
        state: &mut FrameState,
    ) -> QueryResult<api::frame::LookupResponse> {
        Ok(api::frame::LookupResponse {
            transform: lookup_transform(state, &request),
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
                parent_frame_id.clone(),
                child_frame_id.clone(),
                transform,
                Some(stamp),
            ))
        });
        sorted_transforms(static_transforms.chain(dynamic_transforms))
    }
}

#[cfg(test)]
mod tests {
    use nalgebra::{Quaternion, UnitQuaternion};
    use phoxal::api;
    use phoxal::model::structure::Structure;
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

    #[test]
    fn static_chain_lookup_composes_yaw() -> Result<()> {
        let config = FrameConfig::from_structure(&Structure::from_urdf_str(
            r#"
            <robot name="test">
              <link name="base_link"/>
              <joint name="arm_mount" type="fixed">
                <parent link="base_link"/>
                <child link="arm_link"/>
                <origin xyz="0 0 0" rpy="0 0 1.5707963267948966"/>
              </joint>
              <link name="arm_link"/>
            </robot>
            "#,
        )?)?;
        let state = state_from_config(&config, BTreeMap::new());

        let transform = lookup_transform(
            &state,
            &api::frame::LookupRequest {
                target_frame_id: "base_link".to_string(),
                source_frame_id: "arm_link".to_string(),
                at: Some(at(0)),
            },
        )
        .expect("static lookup should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp, None);
        Ok(())
    }

    #[test]
    fn dynamic_joint_lookup_uses_nearest_sample() -> Result<()> {
        let config = single_dynamic_config()?;
        let wheel = "wheel_link".to_string();
        let (_, meta) = config.parent_by_child.get(&wheel).expect("wheel metadata");
        let mut buffer = RingBuffer::new(BUFFER_WINDOW, BUFFER_MAX_ENTRIES);
        buffer.push(
            at(100),
            joint_transform(meta, &joint_state(FRAC_PI_4)).expect("joint transform"),
        );
        buffer.push(
            at(200),
            joint_transform(meta, &joint_state(FRAC_PI_2)).expect("joint transform"),
        );

        let state = state_from_config(&config, BTreeMap::from([(wheel.clone(), buffer)]));
        let transform = lookup_transform(
            &state,
            &api::frame::LookupRequest {
                target_frame_id: "base_link".to_string(),
                source_frame_id: wheel,
                at: Some(at(175)),
            },
        )
        .expect("dynamic lookup should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp, Some(at(200)));
        Ok(())
    }

    #[test]
    fn lookup_returns_none_for_unknown_or_out_of_range_frames() -> Result<()> {
        let config = single_dynamic_config()?;
        let wheel = "wheel_link".to_string();
        let (_, meta) = config.parent_by_child.get(&wheel).expect("wheel metadata");
        let mut buffer = RingBuffer::new(BUFFER_WINDOW, BUFFER_MAX_ENTRIES);
        buffer.push(
            at(100),
            joint_transform(meta, &joint_state(0.0)).expect("joint transform"),
        );
        let state = state_from_config(&config, BTreeMap::from([(wheel.clone(), buffer)]));

        assert!(
            lookup_transform(
                &state,
                &api::frame::LookupRequest {
                    target_frame_id: "base_link".to_string(),
                    source_frame_id: "missing".to_string(),
                    at: Some(at(100)),
                },
            )
            .is_none()
        );
        assert!(
            lookup_transform(
                &state,
                &api::frame::LookupRequest {
                    target_frame_id: "base_link".to_string(),
                    source_frame_id: wheel,
                    at: Some(
                        at(100).saturating_add(BUFFER_WINDOW + std::time::Duration::from_nanos(1))
                    ),
                },
            )
            .is_none()
        );
        Ok(())
    }

    #[test]
    fn lookup_at_or_after_newest_sample_uses_latest_within_tolerance() -> Result<()> {
        let config = single_dynamic_config()?;
        let wheel = "wheel_link".to_string();
        let (_, meta) = config.parent_by_child.get(&wheel).expect("wheel metadata");
        let mut buffer = RingBuffer::new(BUFFER_WINDOW, BUFFER_MAX_ENTRIES);
        buffer.push(
            at(100),
            joint_transform(meta, &joint_state(FRAC_PI_4)).expect("joint transform"),
        );
        buffer.push(
            at(200),
            joint_transform(meta, &joint_state(FRAC_PI_2)).expect("joint transform"),
        );
        let state = state_from_config(&config, BTreeMap::from([(wheel.clone(), buffer)]));

        let transform = lookup_transform(
            &state,
            &api::frame::LookupRequest {
                target_frame_id: "base_link".to_string(),
                source_frame_id: wheel,
                at: Some(at(250)),
            },
        )
        .expect("latest within tolerance should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp, Some(at(200)));
        Ok(())
    }

    #[test]
    fn latest_lookup_uses_newest_dynamic_sample() -> Result<()> {
        let config = single_dynamic_config()?;
        let wheel = "wheel_link".to_string();
        let (_, meta) = config.parent_by_child.get(&wheel).expect("wheel metadata");
        let mut buffer = RingBuffer::new(BUFFER_WINDOW, BUFFER_MAX_ENTRIES);
        buffer.push(
            at(100),
            joint_transform(meta, &joint_state(FRAC_PI_4)).expect("joint transform"),
        );
        buffer.push(
            at(200),
            joint_transform(meta, &joint_state(FRAC_PI_2)).expect("joint transform"),
        );
        let state = state_from_config(&config, BTreeMap::from([(wheel.clone(), buffer)]));

        let transform = lookup_transform(
            &state,
            &api::frame::LookupRequest {
                target_frame_id: "base_link".to_string(),
                source_frame_id: wheel,
                at: None,
            },
        )
        .expect("latest lookup should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp, Some(at(200)));
        Ok(())
    }

    #[test]
    fn ring_buffer_evicts_entries_outside_time_window_and_cap() {
        let mut buffer = RingBuffer::new(std::time::Duration::from_nanos(5), 3);

        for ticks in 0..10 {
            buffer.push(at(ticks), ticks);
        }

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.entries().front().unwrap().0, at(7));
        assert_eq!(buffer.entries().back().unwrap().0, at(9));
    }

    /// A replaced world discards the retained history rather than comparing
    /// against it: instants from the previous timeline are not "old", they are
    /// incomparable (#952 - the `frame` acceptance criterion).
    #[test]
    fn ring_buffer_is_timeline_scoped_across_a_reset() {
        let replaced = phoxal::bus::TimelineId::mint();
        let mut buffer = RingBuffer::new(std::time::Duration::from_secs(1), 8);
        buffer.push(RobotInstant::new(replaced, 100), 1_u64);
        assert_eq!(buffer.len(), 1);
        assert_eq!(
            buffer.nearest(at(100)),
            None,
            "a lookup on the current timeline must not resolve against a replaced world"
        );

        buffer.push(at(10), 2);
        assert_eq!(
            buffer.len(),
            1,
            "the replaced world's samples are discarded, not aged out"
        );
        assert_eq!(buffer.nearest(at(10)), Some((at(10), 2)));
    }

    fn state_from_config(
        config: &FrameConfig,
        dynamics: BTreeMap<String, RingBuffer<Isometry3<f64>>>,
    ) -> FrameState {
        FrameState {
            static_transforms: config.static_transforms.clone(),
            parent_by_child: config.parent_by_child.clone(),
            dynamic_joints: config.dynamic_joints.clone(),
            buffers: dynamics,
            published_static: false,
        }
    }

    fn single_dynamic_config() -> Result<FrameConfig> {
        FrameConfig::from_structure(&Structure::from_urdf_str(
            r#"
            <robot name="test">
              <link name="base_link"/>
              <joint name="wheel_joint" type="continuous">
                <parent link="base_link"/>
                <child link="wheel_link"/>
                <origin xyz="0 0 0" rpy="0 0 0"/>
                <axis xyz="0 0 1"/>
              </joint>
              <link name="wheel_link"/>
            </robot>
            "#,
        )?)
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
        assert_close(yaw, expected_yaw);
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "expected {expected}, got {actual}"
        );
    }
}
