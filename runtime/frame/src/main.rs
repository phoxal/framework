//! `frame` - maintain the robot transform tree and serve frame lookups.
//!
//! A scheduled runtime with a concurrent snapshot server. It builds the link
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

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use anyhow::{Result, bail};
use nalgebra::{Isometry3, Quaternion, Translation3, Unit, UnitQuaternion, Vector3};
use phoxal::api::y2026_1 as api;
use phoxal::model::structure::{Joint as UrdfJoint, JointType, Pose, Structure};
use phoxal::model::v1::Robot;
use phoxal::prelude::*;

const BUFFER_WINDOW_NS: u64 = 5_000_000_000;
const BUFFER_MAX_ENTRIES: usize = 16_384;

#[derive(Clone)]
struct FrameConfig {
    static_transforms: BTreeMap<String, api::frame::FrameTransform>,
    parent_by_child: BTreeMap<String, (String, JointMeta)>,
    dynamic_joints: Vec<DynamicJoint>,
}

impl FrameConfig {
    fn from_robot(robot: &Robot) -> Result<Self> {
        Self::from_structure(&robot.structure)
    }

    fn from_structure(structure: &Structure) -> Result<Self> {
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
                    transform_from_isometry(parent_frame_id, child_frame_id, meta.origin, None),
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
struct DynamicJoint {
    joint_id: String,
    child_frame_id: String,
}

#[derive(Clone, Debug)]
struct JointMeta {
    joint_id: String,
    joint_type: FrameJointType,
    origin: Isometry3<f64>,
    axis_xyz: [f64; 3],
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
enum FrameJointType {
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

#[derive(Clone)]
struct FrameSnapshot {
    statics: Arc<BTreeMap<String, api::frame::FrameTransform>>,
    parent_by_child: Arc<BTreeMap<String, (String, JointMeta)>>,
    dynamics: Arc<BTreeMap<String, Arc<RingBuffer<Isometry3<f64>>>>>,
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "frame", api = y2026_1)]
struct Frame {
    static_transforms: Arc<BTreeMap<String, api::frame::FrameTransform>>,
    parent_by_child: Arc<BTreeMap<String, (String, JointMeta)>>,
    dynamic_joints: Vec<DynamicJoint>,
    buffers: BTreeMap<String, RingBuffer<Isometry3<f64>>>,
    joints: Vec<Subscriber<api::joint::JointState>>,
    tree: Publisher<api::frame::Tree>,
    static_pub: Publisher<api::frame::StaticTransforms>,
    published_static: bool,
}

#[phoxal::runtime]
impl Frame {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let config = FrameConfig::from_robot(ctx.robot()?)?;

        let mut joints = Vec::with_capacity(config.dynamic_joints.len());
        let mut buffers = BTreeMap::new();
        for dynamic in &config.dynamic_joints {
            joints.push(
                ctx.subscribe(api::topic::new().joint(&dynamic.joint_id).state())
                    .subscriber()
                    .await?,
            );
            buffers.insert(
                dynamic.child_frame_id.clone(),
                RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES),
            );
        }

        Ok(Self {
            static_transforms: Arc::new(config.static_transforms),
            parent_by_child: Arc::new(config.parent_by_child),
            dynamic_joints: config.dynamic_joints,
            buffers,
            joints,
            tree: ctx.publisher(api::topic::new().frame().tree()).await?,
            static_pub: ctx
                .publisher(api::topic::new().frame().static_transforms())
                .await?,
            published_static: false,
        })
    }

    #[step(hz = 50)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        for (subscriber, dynamic) in self.joints.iter_mut().zip(&self.dynamic_joints) {
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
            self.static_pub
                .publish_at(
                    step.time(),
                    api::frame::StaticTransforms {
                        transforms: sorted_transforms(self.static_transforms.values().cloned()),
                    },
                )
                .await?;
            self.published_static = true;
        }

        self.tree
            .publish_at(
                step.time(),
                api::frame::Tree {
                    transforms: self.tree_transforms(),
                },
            )
            .await?;
        Ok(())
    }

    #[server_snapshot(topic = api::topic::new().frame().lookup())]
    async fn lookup(
        state: Snapshot<FrameSnapshot>,
        request: api::frame::LookupRequest,
    ) -> ServerResult<api::frame::LookupResponse> {
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
            Some(transform_from_isometry(
                parent_frame_id.clone(),
                child_frame_id.clone(),
                transform,
                Some(stamp_ns),
            ))
        });
        sorted_transforms(static_transforms.chain(dynamic_transforms))
    }
}

#[derive(Clone, Debug)]
struct RingBuffer<T> {
    window_ns: u64,
    max_entries: usize,
    entries: VecDeque<(u64, T)>,
}

impl<T> RingBuffer<T> {
    fn new(window_ns: u64, max_entries: usize) -> Self {
        Self {
            window_ns,
            max_entries,
            entries: VecDeque::with_capacity(max_entries.min(256)),
        }
    }

    fn push(&mut self, timestamp_ns: u64, value: T) {
        if self.max_entries == 0 {
            return;
        }
        while self.entries.front().is_some_and(|(entry_timestamp_ns, _)| {
            entry_timestamp_ns.saturating_add(self.window_ns) < timestamp_ns
        }) {
            self.entries.pop_front();
        }
        while self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back((timestamp_ns, value));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

impl<T: Clone> RingBuffer<T> {
    fn latest(&self) -> Option<(u64, T)> {
        self.entries
            .back()
            .map(|(timestamp_ns, value)| (*timestamp_ns, value.clone()))
    }

    fn nearest(&self, timestamp_ns: u64) -> Option<(u64, T)> {
        let (oldest_available_ns, _) = self.entries.front()?;
        if timestamp_ns < *oldest_available_ns {
            return None;
        }
        let (newest_available_ns, _) = self.entries.back()?;
        if timestamp_ns > *newest_available_ns {
            return (timestamp_ns.saturating_sub(*newest_available_ns) <= self.window_ns)
                .then(|| self.latest())
                .flatten();
        }
        self.entries
            .iter()
            .min_by_key(|(entry_timestamp_ns, _)| entry_timestamp_ns.abs_diff(timestamp_ns))
            .map(|(entry_timestamp_ns, value)| (*entry_timestamp_ns, value.clone()))
    }
}

fn lookup_transform(
    snapshot: &FrameSnapshot,
    request: &api::frame::LookupRequest,
) -> Option<api::frame::FrameTransform> {
    let target = &request.target_frame_id;
    let source = &request.source_frame_id;

    if !known_frame(
        target,
        &snapshot.statics,
        &snapshot.dynamics,
        &snapshot.parent_by_child,
    ) || !known_frame(
        source,
        &snapshot.statics,
        &snapshot.dynamics,
        &snapshot.parent_by_child,
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

    let lca = common_ancestor(target, source, &snapshot.parent_by_child)?;
    let (target_stamp, lca_to_target) = transform_from_ancestor_to_descendant(
        &lca,
        target,
        request.at_ns,
        &snapshot.statics,
        &snapshot.dynamics,
        &snapshot.parent_by_child,
    )?;
    let (source_stamp, lca_to_source) = transform_from_ancestor_to_descendant(
        &lca,
        source,
        request.at_ns,
        &snapshot.statics,
        &snapshot.dynamics,
        &snapshot.parent_by_child,
    )?;

    let stamp_ns = target_stamp.into_iter().chain(source_stamp).max();
    Some(transform_from_isometry(
        target.clone(),
        source.clone(),
        lca_to_target.inverse() * lca_to_source,
        stamp_ns,
    ))
}

fn known_frame(
    frame_id: &str,
    statics: &BTreeMap<String, api::frame::FrameTransform>,
    dynamics: &BTreeMap<String, Arc<RingBuffer<Isometry3<f64>>>>,
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
    at_ns: Option<u64>,
    statics: &BTreeMap<String, api::frame::FrameTransform>,
    dynamics: &BTreeMap<String, Arc<RingBuffer<Isometry3<f64>>>>,
    parent_by_child: &BTreeMap<String, (String, JointMeta)>,
) -> Option<(Option<u64>, Isometry3<f64>)> {
    let mut child_to_parent_edges = Vec::new();
    let mut current = descendant.to_string();

    while current != ancestor {
        let (parent, _) = parent_by_child.get(&current)?;
        child_to_parent_edges.push(edge_transform(&current, at_ns, statics, dynamics)?);
        current = parent.clone();
    }

    let mut stamp_ns = None;
    let mut transform = Isometry3::identity();
    for (edge_stamp_ns, edge) in child_to_parent_edges.into_iter().rev() {
        stamp_ns = stamp_ns.into_iter().chain(edge_stamp_ns).max();
        transform *= edge;
    }
    Some((stamp_ns, transform))
}

fn edge_transform(
    child_frame_id: &str,
    at_ns: Option<u64>,
    statics: &BTreeMap<String, api::frame::FrameTransform>,
    dynamics: &BTreeMap<String, Arc<RingBuffer<Isometry3<f64>>>>,
) -> Option<(Option<u64>, Isometry3<f64>)> {
    if let Some(transform) = statics.get(child_frame_id) {
        return Some((None, isometry_from_transform(transform)));
    }
    let buffer = dynamics.get(child_frame_id)?;
    let (stamp_ns, transform) = match at_ns {
        Some(timestamp_ns) => buffer.nearest(timestamp_ns)?,
        None => buffer.latest()?,
    };
    Some((Some(stamp_ns), transform))
}

fn joint_transform(meta: &JointMeta, state: &api::joint::JointState) -> Option<Isometry3<f64>> {
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

fn pose_to_isometry(pose: &Pose) -> Isometry3<f64> {
    Isometry3::from_parts(
        Translation3::new(pose.xyz[0], pose.xyz[1], pose.xyz[2]),
        UnitQuaternion::from_euler_angles(pose.rpy[0], pose.rpy[1], pose.rpy[2]),
    )
}

fn transform_from_isometry(
    parent_frame_id: String,
    child_frame_id: String,
    transform: Isometry3<f64>,
    stamp_ns: Option<u64>,
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
        stamp_ns,
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

fn sorted_transforms(
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

fn main() -> phoxal::Result<()> {
    phoxal::run::<Frame>()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};
    use std::sync::Arc;

    use phoxal::api::ContractBody;
    use phoxal::api::y2026_1 as api;

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
        assert_eq!(buffer.entries.front().unwrap().0, 7);
        assert_eq!(buffer.entries.back().unwrap().0, 9);
    }

    #[test]
    fn emit_apis_reports_contracts() {
        let metadata = phoxal::runtime::runtime_metadata::<Frame>();
        assert_eq!(metadata.artifact.id, "frame");

        let contracts = metadata.required_contracts;
        assert_contract::<api::joint::JointState>(
            &contracts,
            phoxal::runtime::Direction::Subscribe,
        );
        assert_contract::<api::frame::Tree>(&contracts, phoxal::runtime::Direction::Publish);
        assert_contract::<api::frame::StaticTransforms>(
            &contracts,
            phoxal::runtime::Direction::Publish,
        );
        assert_contract::<api::frame::LookupRequest>(
            &contracts,
            phoxal::runtime::Direction::ServerRequest,
        );
        assert_contract::<api::frame::LookupResponse>(
            &contracts,
            phoxal::runtime::Direction::ServerResponse,
        );
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

    fn assert_contract<B>(
        contracts: &[phoxal::runtime::emit::ContractView],
        direction: phoxal::runtime::Direction,
    ) where
        B: ContractBody,
    {
        assert!(
            contracts.iter().any(|c| {
                c.family == B::FAMILY && c.topic == B::TOPIC && c.direction == direction
            })
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
