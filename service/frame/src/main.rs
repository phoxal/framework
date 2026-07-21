//! `frame` - maintain the robot transform tree and serve frame lookups.
//!
//! A scheduled participant with a concurrent snapshot server. It builds the link
//! tree from the robot model (D33): fixed joints become static transforms, while
//! movable joints (revolute, continuous, prismatic) are tracked dynamically.
//! It subscribes to the per-joint `joint/<id>/state` topic for each movable joint
//! and folds each sample into the joint origin to produce a child-to-parent
//! transform, buffered in a time-windowed ring buffer per child frame.
//! Each step it publishes the latest combined tree on `frame/tree`, and emits the
//! static transforms once on `frame/static_transforms`.
//! A `#[server_snapshot]` serves `frame/lookup` concurrently against the committed
//! snapshot, composing the transform between any two known frames through their
//! lowest common ancestor; it returns no transform for unknown frames or for
//! timestamps outside a dynamic frame's buffered window.
//! Floating, planar, and spherical joints are not supported and fail setup.

mod config;
mod ring_buffer;
mod transform;

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use nalgebra::Isometry3;
use phoxal::api;
use phoxal::prelude::*;

use crate::config::{DynamicJoint, FrameConfig, JointMeta};
use crate::ring_buffer::RingBuffer;
use crate::transform::{joint_transform, lookup_transform, sorted_transforms};

const BUFFER_WINDOW_NS: u64 = 5_000_000_000;
const BUFFER_MAX_ENTRIES: usize = 16_384;

#[derive(Clone)]
struct FrameSnapshot {
    statics: Arc<BTreeMap<String, api::frame::FrameTransform>>,
    parent_by_child: Arc<BTreeMap<String, (String, JointMeta)>>,
    dynamics: Arc<BTreeMap<String, Arc<RingBuffer<Isometry3<f64>>>>>,
}

#[derive(phoxal::Api)]
struct Api {
    joints: Vec<Subscriber<api::joint::JointState>>,
    tree: Publisher<api::frame::Tree>,
    static_pub: Publisher<api::frame::StaticTransforms>,
    lookup: Server<api::frame::LookupRequest, api::frame::LookupResponse>,
}

#[phoxal::service(id = "frame", config = ())]
struct Frame {
    static_transforms: Arc<BTreeMap<String, api::frame::FrameTransform>>,
    parent_by_child: Arc<BTreeMap<String, (String, JointMeta)>>,
    dynamic_joints: Vec<DynamicJoint>,
    buffers: BTreeMap<String, RingBuffer<Isometry3<f64>>>,
    published_static: bool,
}

#[phoxal::behavior]
impl Frame {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        let config = FrameConfig::from_robot(ctx.robot()?)?;

        let mut joints = Vec::with_capacity(config.dynamic_joints.len());
        let mut buffers = BTreeMap::new();
        for dynamic in &config.dynamic_joints {
            joints.push(
                ctx.subscriber(api::topic::new().joint(&dynamic.joint_id).state(), 32)
                    .await?,
            );
            buffers.insert(
                dynamic.child_frame_id.clone(),
                RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES),
            );
        }

        // Frame OWNS the `frame` node (tree, static transforms, and the
        // `frame/lookup` query it serves below) -> owner (`internal`) builder;
        // joint states are CONSUMED via the public builder.
        let tree = ctx
            .publisher(api::topic::internal::new(cap).frame().tree())
            .await?;
        let static_pub = ctx
            .publisher(api::topic::internal::new(cap).frame().static_transforms())
            .await?;
        let lookup = ctx.server(api::topic::new().frame().lookup()).await?;

        Ok((
            Self {
                static_transforms: Arc::new(config.static_transforms),
                parent_by_child: Arc::new(config.parent_by_child),
                dynamic_joints: config.dynamic_joints,
                buffers,
                published_static: false,
            },
            Self::Api {
                joints,
                tree,
                static_pub,
                lookup,
            },
        ))
    }

    #[step(hz = 50)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        for (subscriber, dynamic) in api.joints.iter_mut().zip(&self.dynamic_joints) {
            while let Some(received) = subscriber.try_recv() {
                let Some((_, meta)) = self.parent_by_child.get(&dynamic.child_frame_id) else {
                    continue;
                };
                let Some(transform) = joint_transform(meta, &received.body) else {
                    continue;
                };
                self.buffers
                    .entry(dynamic.child_frame_id.clone())
                    .or_insert_with(|| RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES))
                    .push(received.metadata.produced_at_ns, transform);
            }
        }

        if !self.published_static {
            api.static_pub
                .publish_at(
                    step.time(),
                    api::frame::StaticTransforms {
                        transforms: sorted_transforms(self.static_transforms.values().cloned()),
                    },
                )
                .await?;
            self.published_static = true;
        }

        api.tree
            .publish_at(
                step.time(),
                api::frame::Tree {
                    transforms: self.tree_transforms(),
                },
            )
            .await?;
        Ok(())
    }

    // Concurrent read against the committed snapshot: does not block the step
    // loop. Reads only committed `Snapshot` state, never touches a `Subscriber`
    // (the `joints` field is drained exclusively in `#[step]` above).
    #[server_snapshot(api = lookup)]
    async fn lookup(
        state: Snapshot<FrameSnapshot>,
        api: &Self::Api,
        request: api::frame::LookupRequest,
    ) -> ServerResult<api::frame::LookupResponse> {
        let _ = api;
        Ok(api::frame::LookupResponse {
            transform: lookup_transform(&state, &request),
        })
    }

    #[snapshot]
    fn snapshot(&self) -> FrameSnapshot {
        FrameSnapshot {
            statics: Arc::clone(&self.static_transforms),
            parent_by_child: Arc::clone(&self.parent_by_child),
            dynamics: Arc::new(
                self.buffers
                    .iter()
                    .map(|(frame_id, buffer)| (frame_id.clone(), Arc::new(buffer.clone())))
                    .collect(),
            ),
        }
    }
}

impl Frame {
    fn tree_transforms(&self) -> Vec<api::frame::FrameTransform> {
        let static_transforms = self.static_transforms.values().cloned();
        let dynamic_transforms = self.buffers.iter().filter_map(|(child_frame_id, buffer)| {
            let (stamp_ns, transform) = buffer.latest()?;
            let (parent_frame_id, _) = self.parent_by_child.get(child_frame_id)?;
            Some(transform::transform_from_isometry(
                parent_frame_id.clone(),
                child_frame_id.clone(),
                transform,
                Some(stamp_ns),
            ))
        });
        sorted_transforms(static_transforms.chain(dynamic_transforms))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Frame>()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};
    use std::sync::Arc;

    use nalgebra::{Quaternion, UnitQuaternion};
    use phoxal::api;
    use phoxal::bus::ContractBody;
    use phoxal::model::structure::Structure;
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

    use super::*;

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
        let snapshot = snapshot_from_config(&config, BTreeMap::new());

        let transform = lookup_transform(
            &snapshot,
            &api::frame::LookupRequest {
                target_frame_id: "base_link".to_string(),
                source_frame_id: "arm_link".to_string(),
                at_ns: Some(0),
            },
        )
        .expect("static lookup should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp_ns, None);
        Ok(())
    }

    #[test]
    fn dynamic_joint_lookup_uses_nearest_sample() -> Result<()> {
        let config = single_dynamic_config()?;
        let wheel = "wheel_link".to_string();
        let (_, meta) = config.parent_by_child.get(&wheel).expect("wheel metadata");
        let mut buffer = RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES);
        buffer.push(
            100,
            joint_transform(meta, &joint_state(FRAC_PI_4)).expect("joint transform"),
        );
        buffer.push(
            200,
            joint_transform(meta, &joint_state(FRAC_PI_2)).expect("joint transform"),
        );

        let snapshot = snapshot_from_config(&config, BTreeMap::from([(wheel.clone(), buffer)]));
        let transform = lookup_transform(
            &snapshot,
            &api::frame::LookupRequest {
                target_frame_id: "base_link".to_string(),
                source_frame_id: wheel,
                at_ns: Some(175),
            },
        )
        .expect("dynamic lookup should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp_ns, Some(200));
        Ok(())
    }

    #[test]
    fn lookup_returns_none_for_unknown_or_out_of_range_frames() -> Result<()> {
        let config = single_dynamic_config()?;
        let wheel = "wheel_link".to_string();
        let (_, meta) = config.parent_by_child.get(&wheel).expect("wheel metadata");
        let mut buffer = RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES);
        buffer.push(
            100,
            joint_transform(meta, &joint_state(0.0)).expect("joint transform"),
        );
        let snapshot = snapshot_from_config(&config, BTreeMap::from([(wheel.clone(), buffer)]));

        assert!(
            lookup_transform(
                &snapshot,
                &api::frame::LookupRequest {
                    target_frame_id: "base_link".to_string(),
                    source_frame_id: "missing".to_string(),
                    at_ns: Some(100),
                },
            )
            .is_none()
        );
        assert!(
            lookup_transform(
                &snapshot,
                &api::frame::LookupRequest {
                    target_frame_id: "base_link".to_string(),
                    source_frame_id: wheel,
                    at_ns: Some(100 + BUFFER_WINDOW_NS + 1),
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
        let mut buffer = RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES);
        buffer.push(
            100,
            joint_transform(meta, &joint_state(FRAC_PI_4)).expect("joint transform"),
        );
        buffer.push(
            200,
            joint_transform(meta, &joint_state(FRAC_PI_2)).expect("joint transform"),
        );
        let snapshot = snapshot_from_config(&config, BTreeMap::from([(wheel.clone(), buffer)]));

        let transform = lookup_transform(
            &snapshot,
            &api::frame::LookupRequest {
                target_frame_id: "base_link".to_string(),
                source_frame_id: wheel,
                at_ns: Some(250),
            },
        )
        .expect("latest within tolerance should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp_ns, Some(200));
        Ok(())
    }

    #[test]
    fn latest_lookup_uses_newest_dynamic_sample() -> Result<()> {
        let config = single_dynamic_config()?;
        let wheel = "wheel_link".to_string();
        let (_, meta) = config.parent_by_child.get(&wheel).expect("wheel metadata");
        let mut buffer = RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES);
        buffer.push(
            100,
            joint_transform(meta, &joint_state(FRAC_PI_4)).expect("joint transform"),
        );
        buffer.push(
            200,
            joint_transform(meta, &joint_state(FRAC_PI_2)).expect("joint transform"),
        );
        let snapshot = snapshot_from_config(&config, BTreeMap::from([(wheel.clone(), buffer)]));

        let transform = lookup_transform(
            &snapshot,
            &api::frame::LookupRequest {
                target_frame_id: "base_link".to_string(),
                source_frame_id: wheel,
                at_ns: None,
            },
        )
        .expect("latest lookup should resolve");

        assert_yaw(transform.rotation_quat_xyzw, FRAC_PI_2);
        assert_eq!(transform.stamp_ns, Some(200));
        Ok(())
    }

    #[test]
    fn ring_buffer_evicts_entries_outside_time_window_and_cap() {
        let mut buffer = RingBuffer::new(5, 3);

        for timestamp_ns in 0..10 {
            buffer.push(timestamp_ns, timestamp_ns);
        }

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.entries().front().unwrap().0, 7);
        assert_eq!(buffer.entries().back().unwrap().0, 9);
    }

    #[test]
    fn api_reports_contracts() {
        assert_eq!(<Frame as Participant>::ID, "frame");

        let contracts = <<Frame as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert_contract::<api::joint::JointState>(contracts, ContractRole::Subscribe);
        assert_contract::<api::frame::Tree>(contracts, ContractRole::Publish);
        assert_contract::<api::frame::StaticTransforms>(contracts, ContractRole::Publish);
        assert_contract::<api::frame::LookupRequest>(contracts, ContractRole::Serve);
        assert_contract::<api::frame::LookupResponse>(contracts, ContractRole::Serve);
    }

    fn snapshot_from_config(
        config: &FrameConfig,
        dynamics: BTreeMap<String, RingBuffer<Isometry3<f64>>>,
    ) -> FrameSnapshot {
        FrameSnapshot {
            statics: Arc::new(config.static_transforms.clone()),
            parent_by_child: Arc::new(config.parent_by_child.clone()),
            dynamics: Arc::new(
                dynamics
                    .into_iter()
                    .map(|(frame_id, buffer)| (frame_id, Arc::new(buffer)))
                    .collect(),
            ),
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

    fn assert_contract<B>(contracts: &[phoxal::participant::ApiContractUse], role: ContractRole)
    where
        B: ContractBody,
    {
        assert!(
            contracts
                .iter()
                .any(|c| c.topic == B::TOPIC && c.role == role),
            "expected a {role:?} contract for {} in {contracts:?}",
            B::TOPIC
        );
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
