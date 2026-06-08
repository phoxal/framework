use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use nalgebra::{Isometry3, Quaternion, Translation3, Unit, UnitQuaternion, Vector3};
use phoxal::api::frame::v1::{
    FrameId, FrameLink, FrameLookupRequest, FrameLookupResponse, FrameTransform, Source, Static,
    Tree, data, lookup, r#static, tree,
};
use phoxal::api::joint::v1::{JointId, JointState, Quantity};
use phoxal::bus::pubsub::Stamped;
use phoxal::model::structure::Structure;
use phoxal::runtime::clock::Step;
use phoxal::runtime::step::{Io, Publisher, Runtime, RuntimeInputs};
use phoxal::runtime::{EmptyArgs, QueryOptions, ReadCell, RobotRuntimeArgs};
use phoxal::spatial::frame::{extract_link_transforms, pose_to_isometry};
use tracing::warn;
use urdf_rs::JointType;

const CLOCK_PERIOD: Duration = Duration::from_millis(20);
const BUFFER_WINDOW_NS: u64 = 5_000_000_000;
const BUFFER_MAX_ENTRIES: usize = 16_384;

#[derive(Clone)]
pub struct Config {
    tree: Tree,
    static_transforms: HashMap<FrameId, FrameTransform>,
    parent_by_child: HashMap<FrameId, (FrameId, JointMeta)>,
    dynamic_joints: Vec<DynamicJoint>,
    clock_period: Duration,
}

impl Config {
    pub fn from_args(args: &RobotRuntimeArgs) -> Result<Self> {
        let structure = args.structure()?;
        Self::from_structure(&structure)
    }

    pub fn from_structure(structure: &Structure) -> Result<Self> {
        let root_frame_id = FrameId::new(structure.root_link_name()?);
        let link_transforms = extract_link_transforms(structure)?;
        let joints_by_child = structure
            .joints
            .iter()
            .map(|joint| (joint.child.link.as_str(), joint))
            .collect::<HashMap<_, _>>();

        let mut parent_by_child = HashMap::new();
        let mut static_transforms = HashMap::from([(
            root_frame_id.clone(),
            transform_from_isometry(
                None,
                root_frame_id.clone(),
                Isometry3::identity(),
                Source::Static,
            ),
        )]);
        let mut dynamic_joints = Vec::new();

        for joint in &structure.joints {
            let parent_frame_id = FrameId::new(&joint.parent.link);
            let child_frame_id = FrameId::new(&joint.child.link);
            let meta = JointMeta::from_joint(joint)?;
            parent_by_child.insert(
                child_frame_id.clone(),
                (parent_frame_id.clone(), meta.clone()),
            );

            if meta.joint_type == FrameJointType::Fixed {
                let local =
                    local_static_transform(&link_transforms, &parent_frame_id, &child_frame_id)?;
                static_transforms.insert(
                    child_frame_id.clone(),
                    transform_from_isometry(
                        Some(parent_frame_id),
                        child_frame_id,
                        local,
                        Source::Static,
                    ),
                );
            } else {
                dynamic_joints.push(DynamicJoint {
                    joint_id: meta.joint_id.clone(),
                    child_frame_id,
                });
            }
        }

        let frames = structure
            .links
            .iter()
            .map(|link| {
                let frame_id = FrameId::new(&link.name);
                let parent_frame_id = joints_by_child
                    .get(link.name.as_str())
                    .map(|joint| FrameId::new(&joint.parent.link));
                FrameLink {
                    frame_id,
                    parent_frame_id,
                }
            })
            .collect();

        Ok(Self {
            tree: Tree {
                revision: 0,
                frames,
            },
            static_transforms,
            parent_by_child,
            dynamic_joints,
            clock_period: CLOCK_PERIOD,
        })
    }

    pub const fn clock_period(&self) -> Duration {
        self.clock_period
    }
}

#[derive(Debug, Clone)]
struct DynamicJoint {
    joint_id: JointId,
    child_frame_id: FrameId,
}

#[derive(Debug, Clone)]
struct JointMeta {
    joint_id: JointId,
    joint_type: FrameJointType,
    origin: Isometry3<f64>,
    axis_xyz: [f64; 3],
}

impl JointMeta {
    fn from_joint(joint: &urdf_rs::Joint) -> Result<Self> {
        Ok(Self {
            joint_id: JointId::new(&joint.name),
            joint_type: FrameJointType::from_urdf(&joint.joint_type)?,
            origin: pose_to_isometry(&joint.origin),
            axis_xyz: [joint.axis.xyz[0], joint.axis.xyz[1], joint.axis.xyz[2]],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
                bail!("unsupported frame joint type {:?}", joint_type)
            }
        }
    }
}

pub enum Input {
    Joint {
        joint_id: JointId,
        child_frame_id: FrameId,
        sample: Stamped<JointState>,
    },
}

pub struct FrameView {
    statics: Arc<HashMap<FrameId, FrameTransform>>,
    parent_by_child: Arc<HashMap<FrameId, (FrameId, JointMeta)>>,
    dynamics: Arc<HashMap<FrameId, Arc<RingBuffer<Isometry3<f64>>>>>,
}

pub struct FrameRuntime {
    initial: bool,
    tree: Tree,
    static_transforms: Arc<HashMap<FrameId, FrameTransform>>,
    parent_by_child: Arc<HashMap<FrameId, (FrameId, JointMeta)>>,
    dynamic_by_child: HashMap<FrameId, DynamicJoint>,
    buffers: HashMap<FrameId, RingBuffer<Isometry3<f64>>>,
    published_dynamics: HashMap<FrameId, Arc<RingBuffer<Isometry3<f64>>>>,
    view: ReadCell<FrameView>,
    tree_publisher: Publisher<Stamped<Tree>>,
    static_publisher: Publisher<Stamped<Static>>,
    dynamic_publishers: HashMap<FrameId, Publisher<Stamped<FrameTransform>>>,
}

#[async_trait::async_trait]
impl Runtime for FrameRuntime {
    const RUNTIME_ID: &'static str = "frame";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Config::from_args(common)
    }

    fn clock_period(config: &Self::Config) -> Duration {
        config.clock_period()
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        let tree_publisher = io.publisher::<Stamped<Tree>>(tree::TOPIC).await?;
        let static_publisher = io.publisher::<Stamped<Static>>(r#static::TOPIC).await?;
        let static_transforms = Arc::new(config.static_transforms);
        let parent_by_child = Arc::new(config.parent_by_child);

        let mut dynamic_publishers = HashMap::new();
        let mut buffers = HashMap::new();
        for dynamic in &config.dynamic_joints {
            let joint_id = dynamic.joint_id.clone();
            let child_frame_id = dynamic.child_frame_id.clone();
            io.subscribe::<Stamped<JointState>, _>(
                &phoxal::api::joint::v1::data::path(&joint_id),
                move |sample| Input::Joint {
                    joint_id: joint_id.clone(),
                    child_frame_id: child_frame_id.clone(),
                    sample,
                },
            )
            .await?;
            dynamic_publishers.insert(
                dynamic.child_frame_id.clone(),
                io.publisher::<Stamped<FrameTransform>>(&data::path(&dynamic.child_frame_id))
                    .await?,
            );
            buffers.insert(
                dynamic.child_frame_id.clone(),
                RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES),
            );
        }

        let published_dynamics = initial_published_dynamics(&buffers);
        let view = ReadCell::new(FrameView {
            statics: static_transforms.clone(),
            parent_by_child: parent_by_child.clone(),
            dynamics: Arc::new(published_dynamics.clone()),
        });
        io.serve_query::<FrameLookupRequest, FrameLookupResponse, FrameView, _>(
            lookup::TOPIC,
            view.reader(),
            QueryOptions::max_in_flight(NonZeroUsize::new(16).unwrap()),
            frame_lookup,
        )
        .await?;

        let dynamic_by_child = config
            .dynamic_joints
            .into_iter()
            .map(|dynamic| (dynamic.child_frame_id.clone(), dynamic))
            .collect();

        Ok(Self {
            initial: true,
            tree: config.tree,
            static_transforms,
            parent_by_child,
            dynamic_by_child,
            buffers,
            published_dynamics,
            view,
            tree_publisher,
            static_publisher,
            dynamic_publishers,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        let mut updated = HashMap::new();

        for input in inputs {
            let Input::Joint {
                joint_id,
                child_frame_id,
                sample,
            } = input;
            let Some((_, meta)) = self.parent_by_child.get(&child_frame_id) else {
                warn!(frame_id = %child_frame_id, "frame runtime received joint sample for unknown child frame");
                continue;
            };
            let Some(dynamic) = self.dynamic_by_child.get(&child_frame_id) else {
                warn!(frame_id = %child_frame_id, "frame runtime received joint sample for non-dynamic frame");
                continue;
            };
            if dynamic.joint_id != joint_id {
                warn!(
                    expected_joint_id = %dynamic.joint_id,
                    actual_joint_id = %joint_id,
                    frame_id = %child_frame_id,
                    "frame runtime received joint sample with mismatched joint id"
                );
                continue;
            }
            let Some(transform) = joint_transform(meta, &sample.data) else {
                continue;
            };
            self.buffers
                .entry(child_frame_id.clone())
                .or_insert_with(|| RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES))
                .push(sample.timestamp_ns, transform);
            updated.insert(child_frame_id, (joint_id, sample.timestamp_ns, transform));
        }

        let changed_frame_ids = updated.keys().cloned().collect::<Vec<_>>();
        let should_commit_query_view = self.initial || !changed_frame_ids.is_empty();
        let time_ns = step.tick.time_ns();
        if self.initial {
            self.static_publisher
                .put(&Stamped::new(time_ns, self.static_payload()))
                .await?;
            self.initial = false;
        }

        self.tree_publisher
            .put(&Stamped::new(time_ns, self.tree.clone()))
            .await?;

        for (child_frame_id, (joint_id, timestamp_ns, transform)) in updated {
            let Some((parent_frame_id, _)) = self.parent_by_child.get(&child_frame_id) else {
                continue;
            };
            let Some(publisher) = self.dynamic_publishers.get(&child_frame_id) else {
                warn!(frame_id = %child_frame_id, "frame runtime has no publisher for dynamic frame");
                continue;
            };
            publisher
                .put(&Stamped::new(
                    timestamp_ns,
                    transform_from_isometry(
                        Some(parent_frame_id.clone()),
                        child_frame_id,
                        transform,
                        Source::Joint { joint_id },
                    ),
                ))
                .await?;
        }

        if should_commit_query_view {
            self.commit_query_view(&changed_frame_ids);
        }

        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::step::ScenarioDescriptor] {
        crate::scenarios::SCENARIOS
    }

    async fn run_scenario(
        name: &str,
        _common: &RobotRuntimeArgs,
        _args: &Self::Args,
    ) -> Result<()> {
        crate::scenarios::run(name)
    }
}

impl FrameRuntime {
    fn static_payload(&self) -> Static {
        let mut transforms = self.static_transforms.values().cloned().collect::<Vec<_>>();
        transforms.sort_by(|left, right| left.child_frame_id.cmp(&right.child_frame_id));
        Static { transforms }
    }

    fn commit_query_view(&mut self, changed_frame_ids: &[FrameId]) {
        let view = committed_frame_view(
            self.static_transforms.clone(),
            self.parent_by_child.clone(),
            &self.buffers,
            &mut self.published_dynamics,
            changed_frame_ids,
        );
        self.view.publish(view);
    }
}

fn initial_published_dynamics(
    buffers: &HashMap<FrameId, RingBuffer<Isometry3<f64>>>,
) -> HashMap<FrameId, Arc<RingBuffer<Isometry3<f64>>>> {
    buffers
        .iter()
        .map(|(frame_id, buffer)| (frame_id.clone(), Arc::new(buffer.clone())))
        .collect()
}

fn committed_frame_view(
    statics: Arc<HashMap<FrameId, FrameTransform>>,
    parent_by_child: Arc<HashMap<FrameId, (FrameId, JointMeta)>>,
    buffers: &HashMap<FrameId, RingBuffer<Isometry3<f64>>>,
    published_dynamics: &mut HashMap<FrameId, Arc<RingBuffer<Isometry3<f64>>>>,
    changed_frame_ids: &[FrameId],
) -> FrameView {
    for frame_id in changed_frame_ids {
        if let Some(buffer) = buffers.get(frame_id) {
            published_dynamics.insert(frame_id.clone(), Arc::new(buffer.clone()));
        }
    }

    FrameView {
        statics,
        parent_by_child,
        dynamics: Arc::new(published_dynamics.clone()),
    }
}

#[derive(Debug, Clone)]
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

impl<T: Copy> RingBuffer<T> {
    fn nearest(&self, timestamp_ns: u64) -> Result<T, Box<FrameLookupResponse>> {
        let Some((oldest_available_ns, _)) = self.entries.front() else {
            return Err(Box::new(FrameLookupResponse::ExtrapolationTooNew {
                newest_available_ns: 0,
            }));
        };
        if timestamp_ns < *oldest_available_ns {
            return Err(Box::new(FrameLookupResponse::ExtrapolationTooOld {
                oldest_available_ns: *oldest_available_ns,
            }));
        }

        let Some((newest_available_ns, _)) = self.entries.back() else {
            return Err(Box::new(FrameLookupResponse::ExtrapolationTooNew {
                newest_available_ns: 0,
            }));
        };
        if timestamp_ns > *newest_available_ns {
            return Err(Box::new(FrameLookupResponse::ExtrapolationTooNew {
                newest_available_ns: *newest_available_ns,
            }));
        }

        self.entries
            .iter()
            .min_by_key(|(entry_timestamp_ns, _)| entry_timestamp_ns.abs_diff(timestamp_ns))
            .map(|(_, transform)| *transform)
            .ok_or_else(|| {
                Box::new(FrameLookupResponse::ExtrapolationTooNew {
                    newest_available_ns: *newest_available_ns,
                })
            })
    }
}

fn frame_lookup(view: &FrameView, request: FrameLookupRequest) -> FrameLookupResponse {
    resolve_lookup(
        &request.parent_frame_id,
        &request.child_frame_id,
        request.timestamp_ns,
        &view.statics,
        &view.dynamics,
        &view.parent_by_child,
    )
}

fn resolve_lookup(
    parent: &FrameId,
    child: &FrameId,
    timestamp_ns: u64,
    statics: &HashMap<FrameId, FrameTransform>,
    dynamics: &HashMap<FrameId, Arc<RingBuffer<Isometry3<f64>>>>,
    parent_by_child: &HashMap<FrameId, (FrameId, JointMeta)>,
) -> FrameLookupResponse {
    if !known_frame(parent, statics, dynamics, parent_by_child) {
        return FrameLookupResponse::UnknownFrame {
            frame_id: parent.clone(),
        };
    }
    if !known_frame(child, statics, dynamics, parent_by_child) {
        return FrameLookupResponse::UnknownFrame {
            frame_id: child.clone(),
        };
    }

    if parent == child {
        return lookup_ok(parent, child, timestamp_ns, Isometry3::identity());
    }

    let Some(lca) = common_ancestor(parent, child, parent_by_child) else {
        return FrameLookupResponse::DisconnectedTree {
            parent_frame_id: parent.clone(),
            child_frame_id: child.clone(),
        };
    };

    let lca_to_parent = match transform_from_ancestor_to_descendant(
        &lca,
        parent,
        timestamp_ns,
        statics,
        dynamics,
        parent_by_child,
    ) {
        Ok(transform) => transform,
        Err(response) => return *response,
    };
    let lca_to_child = match transform_from_ancestor_to_descendant(
        &lca,
        child,
        timestamp_ns,
        statics,
        dynamics,
        parent_by_child,
    ) {
        Ok(transform) => transform,
        Err(response) => return *response,
    };

    lookup_ok(
        parent,
        child,
        timestamp_ns,
        lca_to_parent.inverse() * lca_to_child,
    )
}

fn lookup_ok(
    parent: &FrameId,
    child: &FrameId,
    timestamp_ns: u64,
    transform: Isometry3<f64>,
) -> FrameLookupResponse {
    FrameLookupResponse::Ok {
        parent_frame_id: parent.clone(),
        child_frame_id: child.clone(),
        timestamp_ns,
        transform: transform_from_isometry(
            Some(parent.clone()),
            child.clone(),
            transform,
            Source::Lookup,
        ),
    }
}

fn known_frame(
    frame_id: &FrameId,
    statics: &HashMap<FrameId, FrameTransform>,
    dynamics: &HashMap<FrameId, Arc<RingBuffer<Isometry3<f64>>>>,
    parent_by_child: &HashMap<FrameId, (FrameId, JointMeta)>,
) -> bool {
    statics.contains_key(frame_id)
        || dynamics.contains_key(frame_id)
        || parent_by_child.contains_key(frame_id)
        || parent_by_child
            .values()
            .any(|(parent_frame_id, _)| parent_frame_id == frame_id)
}

fn common_ancestor(
    parent: &FrameId,
    child: &FrameId,
    parent_by_child: &HashMap<FrameId, (FrameId, JointMeta)>,
) -> Option<FrameId> {
    let parent_ancestors = ancestors(parent, parent_by_child);
    let mut current = child.clone();
    loop {
        if parent_ancestors.contains(&current) {
            return Some(current);
        }
        let (next, _) = parent_by_child.get(&current)?;
        current = next.clone();
    }
}

fn ancestors(
    frame_id: &FrameId,
    parent_by_child: &HashMap<FrameId, (FrameId, JointMeta)>,
) -> HashSet<FrameId> {
    let mut ancestors = HashSet::new();
    let mut current = frame_id.clone();
    loop {
        ancestors.insert(current.clone());
        let Some((parent, _)) = parent_by_child.get(&current) else {
            return ancestors;
        };
        current = parent.clone();
    }
}

fn transform_from_ancestor_to_descendant(
    ancestor: &FrameId,
    descendant: &FrameId,
    timestamp_ns: u64,
    statics: &HashMap<FrameId, FrameTransform>,
    dynamics: &HashMap<FrameId, Arc<RingBuffer<Isometry3<f64>>>>,
    parent_by_child: &HashMap<FrameId, (FrameId, JointMeta)>,
) -> Result<Isometry3<f64>, Box<FrameLookupResponse>> {
    let mut child_to_parent_edges = Vec::new();
    let mut current = descendant.clone();

    while current != *ancestor {
        let Some((parent, _)) = parent_by_child.get(&current) else {
            return Err(Box::new(FrameLookupResponse::DisconnectedTree {
                parent_frame_id: ancestor.clone(),
                child_frame_id: descendant.clone(),
            }));
        };
        let edge = edge_transform(&current, timestamp_ns, statics, dynamics)?;
        child_to_parent_edges.push(edge);
        current = parent.clone();
    }

    let mut transform = Isometry3::identity();
    for edge in child_to_parent_edges.into_iter().rev() {
        transform *= edge;
    }
    Ok(transform)
}

fn edge_transform(
    child_frame_id: &FrameId,
    timestamp_ns: u64,
    statics: &HashMap<FrameId, FrameTransform>,
    dynamics: &HashMap<FrameId, Arc<RingBuffer<Isometry3<f64>>>>,
) -> Result<Isometry3<f64>, Box<FrameLookupResponse>> {
    if let Some(transform) = statics.get(child_frame_id) {
        return Ok(isometry_from_transform(transform));
    }
    if let Some(buffer) = dynamics.get(child_frame_id) {
        return buffer.as_ref().nearest(timestamp_ns);
    }
    Err(Box::new(FrameLookupResponse::UnknownFrame {
        frame_id: child_frame_id.clone(),
    }))
}

fn local_static_transform(
    link_transforms: &HashMap<String, Isometry3<f64>>,
    parent_frame_id: &FrameId,
    child_frame_id: &FrameId,
) -> Result<Isometry3<f64>> {
    let parent = link_transforms
        .get(&parent_frame_id.0)
        .ok_or_else(|| anyhow!("missing static transform for parent frame '{parent_frame_id}'"))?;
    let child = link_transforms
        .get(&child_frame_id.0)
        .ok_or_else(|| anyhow!("missing static transform for child frame '{child_frame_id}'"))?;
    Ok(parent.inverse() * child)
}

fn joint_transform(meta: &JointMeta, state: &JointState) -> Option<Isometry3<f64>> {
    match (meta.joint_type, state.quantity) {
        (FrameJointType::Fixed, _) => Some(meta.origin),
        (FrameJointType::Revolute | FrameJointType::Continuous, Quantity::AngleRad) => {
            let Some(axis) = joint_axis(meta) else {
                warn!(joint_id = %meta.joint_id, "frame runtime skipped joint sample with zero rotation axis");
                return None;
            };
            Some(
                meta.origin
                    * Isometry3::from_parts(
                        Translation3::identity(),
                        UnitQuaternion::from_axis_angle(&axis, state.value),
                    ),
            )
        }
        (FrameJointType::Prismatic, Quantity::LinearM) => {
            let Some(axis) = joint_axis(meta) else {
                warn!(joint_id = %meta.joint_id, "frame runtime skipped joint sample with zero translation axis");
                return None;
            };
            let axis = axis.into_inner();
            Some(
                meta.origin
                    * Isometry3::from_parts(
                        Translation3::new(
                            axis.x * state.value,
                            axis.y * state.value,
                            axis.z * state.value,
                        ),
                        UnitQuaternion::identity(),
                    ),
            )
        }
        (FrameJointType::Revolute | FrameJointType::Continuous, Quantity::LinearM)
        | (FrameJointType::Prismatic, Quantity::AngleRad) => {
            warn!(joint_id = %meta.joint_id, "frame runtime skipped joint sample with incompatible quantity");
            None
        }
    }
}

fn joint_axis(meta: &JointMeta) -> Option<Unit<Vector3<f64>>> {
    Unit::try_new(
        Vector3::new(meta.axis_xyz[0], meta.axis_xyz[1], meta.axis_xyz[2]),
        f64::EPSILON,
    )
}

fn transform_from_isometry(
    parent_frame_id: Option<FrameId>,
    child_frame_id: FrameId,
    transform: Isometry3<f64>,
    source: Source,
) -> FrameTransform {
    let q = transform.rotation.quaternion();
    FrameTransform {
        parent_frame_id,
        child_frame_id,
        translation_m: [
            transform.translation.x,
            transform.translation.y,
            transform.translation.z,
        ],
        rotation_xyzw: [q.i, q.j, q.k, q.w],
        source,
    }
}

fn isometry_from_transform(transform: &FrameTransform) -> Isometry3<f64> {
    Isometry3::from_parts(
        Translation3::new(
            transform.translation_m[0],
            transform.translation_m[1],
            transform.translation_m[2],
        ),
        UnitQuaternion::from_quaternion(Quaternion::new(
            transform.rotation_xyzw[3],
            transform.rotation_xyzw[0],
            transform.rotation_xyzw[1],
            transform.rotation_xyzw[2],
        )),
    )
}

#[cfg(test)]
mod tests {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};

    use super::*;

    const EPSILON: f64 = 1e-9;
    type DynamicStateFixture = (Config, HashMap<FrameId, Arc<RingBuffer<Isometry3<f64>>>>);

    #[test]
    fn static_chain_lookup_composes_yaw() -> Result<()> {
        let config = Config::from_structure(&Structure::from_urdf_str(
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

        let response = resolve_lookup(
            &FrameId::new("base_link"),
            &FrameId::new("arm_link"),
            0,
            &config.static_transforms,
            &HashMap::new(),
            &config.parent_by_child,
        );

        let transform = ok_transform(response);
        assert_yaw(transform.rotation_xyzw, FRAC_PI_2);
        Ok(())
    }

    #[test]
    fn dynamic_joint_lookup_composes_yaw() -> Result<()> {
        let config = Config::from_structure(&Structure::from_urdf_str(
            r#"
            <robot name="test">
              <link name="base_link"/>
              <joint name="wheel_joint" type="revolute">
                <parent link="base_link"/>
                <child link="wheel_link"/>
                <origin xyz="0 0 0" rpy="0 0 0"/>
                <axis xyz="0 0 1"/>
                <limit lower="-3.14" upper="3.14" effort="1" velocity="1"/>
              </joint>
              <link name="wheel_link"/>
            </robot>
            "#,
        )?)?;
        let wheel = FrameId::new("wheel_link");
        let (_, meta) = config.parent_by_child.get(&wheel).expect("wheel metadata");
        let mut buffer = RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES);
        buffer.push(
            1_000_000_000,
            joint_transform(
                meta,
                &JointState {
                    value: FRAC_PI_4,
                    quantity: Quantity::AngleRad,
                },
            )
            .expect("joint transform"),
        );
        let dynamics = HashMap::from([(wheel.clone(), Arc::new(buffer))]);

        let response = resolve_lookup(
            &FrameId::new("base_link"),
            &wheel,
            1_000_000_000,
            &config.static_transforms,
            &dynamics,
            &config.parent_by_child,
        );

        let transform = ok_transform(response);
        assert_yaw(transform.rotation_xyzw, FRAC_PI_4);
        Ok(())
    }

    #[test]
    fn unknown_frame_reports_frame_id() -> Result<()> {
        let config = single_dynamic_config()?;

        let response = resolve_lookup(
            &FrameId::new("base_link"),
            &FrameId::new("missing_link"),
            0,
            &config.static_transforms,
            &HashMap::new(),
            &config.parent_by_child,
        );

        assert_eq!(
            response,
            FrameLookupResponse::UnknownFrame {
                frame_id: FrameId::new("missing_link")
            }
        );
        Ok(())
    }

    #[test]
    fn newer_than_buffer_reports_newest_available_timestamp() -> Result<()> {
        let (config, dynamics) = single_sample_dynamic_state()?;

        let response = resolve_lookup(
            &FrameId::new("base_link"),
            &FrameId::new("wheel_link"),
            2_000_000_000,
            &config.static_transforms,
            &dynamics,
            &config.parent_by_child,
        );

        assert_eq!(
            response,
            FrameLookupResponse::ExtrapolationTooNew {
                newest_available_ns: 1_000_000_000
            }
        );
        Ok(())
    }

    #[test]
    fn older_than_buffer_reports_oldest_available_timestamp() -> Result<()> {
        let (config, dynamics) = single_sample_dynamic_state()?;

        let response = resolve_lookup(
            &FrameId::new("base_link"),
            &FrameId::new("wheel_link"),
            999_999_999,
            &config.static_transforms,
            &dynamics,
            &config.parent_by_child,
        );

        assert_eq!(
            response,
            FrameLookupResponse::ExtrapolationTooOld {
                oldest_available_ns: 1_000_000_000
            }
        );
        Ok(())
    }

    #[test]
    fn disconnected_trees_report_both_frames() -> Result<()> {
        let left = Config::from_structure(&Structure::from_urdf_str(
            r#"
            <robot name="left">
              <link name="left_root"/>
              <joint name="left_joint" type="fixed">
                <parent link="left_root"/>
                <child link="left_child"/>
              </joint>
              <link name="left_child"/>
            </robot>
            "#,
        )?)?;
        let right = Config::from_structure(&Structure::from_urdf_str(
            r#"
            <robot name="right">
              <link name="right_root"/>
              <joint name="right_joint" type="fixed">
                <parent link="right_root"/>
                <child link="right_child"/>
              </joint>
              <link name="right_child"/>
            </robot>
            "#,
        )?)?;
        let mut statics = left.static_transforms;
        statics.extend(right.static_transforms);
        let mut parent_by_child = left.parent_by_child;
        parent_by_child.extend(right.parent_by_child);

        let response = resolve_lookup(
            &FrameId::new("left_child"),
            &FrameId::new("right_child"),
            0,
            &statics,
            &HashMap::new(),
            &parent_by_child,
        );

        assert_eq!(
            response,
            FrameLookupResponse::DisconnectedTree {
                parent_frame_id: FrameId::new("left_child"),
                child_frame_id: FrameId::new("right_child")
            }
        );
        Ok(())
    }

    #[test]
    fn identity_lookup_returns_identity_transform() -> Result<()> {
        let config = single_dynamic_config()?;

        let response = resolve_lookup(
            &FrameId::new("base_link"),
            &FrameId::new("base_link"),
            123,
            &config.static_transforms,
            &HashMap::new(),
            &config.parent_by_child,
        );

        let transform = ok_transform(response);
        assert_close(transform.translation_m[0], 0.0);
        assert_close(transform.translation_m[1], 0.0);
        assert_close(transform.translation_m[2], 0.0);
        assert_yaw(transform.rotation_xyzw, 0.0);
        Ok(())
    }

    #[test]
    fn frame_lookup_reads_frozen_view_responses() {
        let base = FrameId::new("base_link");
        let camera = FrameId::new("camera_link");
        let isolated_root = FrameId::new("isolated_root");
        let isolated_child = FrameId::new("isolated_child");

        let statics = Arc::new(HashMap::from([
            (
                base.clone(),
                transform_from_isometry(None, base.clone(), Isometry3::identity(), Source::Static),
            ),
            (
                isolated_root.clone(),
                transform_from_isometry(
                    None,
                    isolated_root.clone(),
                    Isometry3::identity(),
                    Source::Static,
                ),
            ),
            (
                isolated_child.clone(),
                transform_from_isometry(
                    Some(isolated_root.clone()),
                    isolated_child.clone(),
                    Isometry3::identity(),
                    Source::Static,
                ),
            ),
        ]));
        let parent_by_child = Arc::new(HashMap::from([
            (
                camera.clone(),
                (base.clone(), test_joint_meta("camera_joint")),
            ),
            (
                isolated_child.clone(),
                (isolated_root.clone(), test_joint_meta("isolated_joint")),
            ),
        ]));
        let mut camera_buffer = RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES);
        camera_buffer.push(100, yaw_isometry(FRAC_PI_4));
        camera_buffer.push(200, yaw_isometry(FRAC_PI_2));
        let view = FrameView {
            statics,
            parent_by_child,
            dynamics: Arc::new(HashMap::from([(camera.clone(), Arc::new(camera_buffer))])),
        };

        let response = frame_lookup(
            &view,
            FrameLookupRequest {
                parent_frame_id: base.clone(),
                child_frame_id: camera.clone(),
                timestamp_ns: 175,
            },
        );
        let FrameLookupResponse::Ok {
            parent_frame_id,
            child_frame_id,
            timestamp_ns,
            transform,
        } = response
        else {
            panic!("expected ok response, got {response:?}");
        };
        assert_eq!(parent_frame_id, base);
        assert_eq!(child_frame_id, camera);
        assert_eq!(timestamp_ns, 175);
        assert_yaw(transform.rotation_xyzw, FRAC_PI_2);

        assert_eq!(
            frame_lookup(
                &view,
                FrameLookupRequest {
                    parent_frame_id: FrameId::new("base_link"),
                    child_frame_id: FrameId::new("camera_link"),
                    timestamp_ns: 99,
                },
            ),
            FrameLookupResponse::ExtrapolationTooOld {
                oldest_available_ns: 100
            }
        );
        assert_eq!(
            frame_lookup(
                &view,
                FrameLookupRequest {
                    parent_frame_id: FrameId::new("base_link"),
                    child_frame_id: FrameId::new("camera_link"),
                    timestamp_ns: 201,
                },
            ),
            FrameLookupResponse::ExtrapolationTooNew {
                newest_available_ns: 200
            }
        );
        assert_eq!(
            frame_lookup(
                &view,
                FrameLookupRequest {
                    parent_frame_id: FrameId::new("base_link"),
                    child_frame_id: FrameId::new("missing_link"),
                    timestamp_ns: 175,
                },
            ),
            FrameLookupResponse::UnknownFrame {
                frame_id: FrameId::new("missing_link")
            }
        );
        assert_eq!(
            frame_lookup(
                &view,
                FrameLookupRequest {
                    parent_frame_id: FrameId::new("base_link"),
                    child_frame_id: FrameId::new("isolated_child"),
                    timestamp_ns: 175,
                },
            ),
            FrameLookupResponse::DisconnectedTree {
                parent_frame_id: FrameId::new("base_link"),
                child_frame_id: FrameId::new("isolated_child")
            }
        );
    }

    #[test]
    fn committing_changed_frame_reuses_unchanged_history_arc() {
        let statics = Arc::new(HashMap::new());
        let parent_by_child = Arc::new(HashMap::new());
        let changed = FrameId::new("changed");
        let unchanged = FrameId::new("unchanged");
        let mut changed_buffer = RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES);
        changed_buffer.push(1, Isometry3::identity());
        let mut unchanged_buffer = RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES);
        unchanged_buffer.push(1, Isometry3::identity());
        let mut buffers = HashMap::from([
            (changed.clone(), changed_buffer),
            (unchanged.clone(), unchanged_buffer),
        ]);
        let mut published_dynamics = initial_published_dynamics(&buffers);
        let before_changed = published_dynamics
            .get(&changed)
            .expect("changed history")
            .clone();
        let before_unchanged = published_dynamics
            .get(&unchanged)
            .expect("unchanged history")
            .clone();

        buffers
            .get_mut(&changed)
            .expect("changed buffer")
            .push(2, Isometry3::translation(1.0, 0.0, 0.0));
        let view = committed_frame_view(
            statics,
            parent_by_child,
            &buffers,
            &mut published_dynamics,
            std::slice::from_ref(&changed),
        );

        let after_changed = view.dynamics.get(&changed).expect("changed history");
        let after_unchanged = view.dynamics.get(&unchanged).expect("unchanged history");
        assert!(!Arc::ptr_eq(&before_changed, after_changed));
        assert!(Arc::ptr_eq(&before_unchanged, after_unchanged));
    }

    #[test]
    fn ring_buffer_evicts_entries_outside_time_window() {
        let mut buffer = RingBuffer::new(5_000_000_000, BUFFER_MAX_ENTRIES);

        for second in 0..=10 {
            buffer.push(second * 1_000_000_000, second);
        }

        assert_eq!(buffer.len(), 6);
        assert!(
            buffer
                .entries
                .iter()
                .all(|(timestamp_ns, _)| { *timestamp_ns >= 5_000_000_000 })
        );
    }

    #[test]
    fn ring_buffer_never_exceeds_defensive_entry_cap() {
        let mut buffer = RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES);

        for index in 0..20_000 {
            buffer.push(index, index);
            assert!(buffer.len() <= BUFFER_MAX_ENTRIES);
        }

        assert_eq!(buffer.len(), BUFFER_MAX_ENTRIES);
    }

    fn single_sample_dynamic_state() -> Result<DynamicStateFixture> {
        let config = single_dynamic_config()?;
        let wheel = FrameId::new("wheel_link");
        let (_, meta) = config.parent_by_child.get(&wheel).expect("wheel metadata");
        let mut buffer = RingBuffer::new(BUFFER_WINDOW_NS, BUFFER_MAX_ENTRIES);
        buffer.push(
            1_000_000_000,
            joint_transform(
                meta,
                &JointState {
                    value: 0.0,
                    quantity: Quantity::AngleRad,
                },
            )
            .expect("joint transform"),
        );
        Ok((config, HashMap::from([(wheel, Arc::new(buffer))])))
    }

    fn single_dynamic_config() -> Result<Config> {
        Config::from_structure(&Structure::from_urdf_str(
            r#"
            <robot name="test">
              <link name="base_link"/>
              <joint name="wheel_joint" type="revolute">
                <parent link="base_link"/>
                <child link="wheel_link"/>
                <origin xyz="0 0 0" rpy="0 0 0"/>
                <axis xyz="0 0 1"/>
                <limit lower="-3.14" upper="3.14" effort="1" velocity="1"/>
              </joint>
              <link name="wheel_link"/>
            </robot>
            "#,
        )?)
    }

    fn ok_transform(response: FrameLookupResponse) -> FrameTransform {
        match response {
            FrameLookupResponse::Ok { transform, .. } => transform,
            other => panic!("expected ok response, got {other:?}"),
        }
    }

    fn test_joint_meta(joint_id: &str) -> JointMeta {
        JointMeta {
            joint_id: JointId::new(joint_id),
            joint_type: FrameJointType::Revolute,
            origin: Isometry3::identity(),
            axis_xyz: [0.0, 0.0, 1.0],
        }
    }

    fn yaw_isometry(yaw: f64) -> Isometry3<f64> {
        Isometry3::from_parts(
            Translation3::identity(),
            UnitQuaternion::from_axis_angle(&Vector3::z_axis(), yaw),
        )
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
