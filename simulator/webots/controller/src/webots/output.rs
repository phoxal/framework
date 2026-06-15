use crate::webots::controller::{Controller, ControllerContract};
use anyhow::{Result, anyhow};
use phoxal::api::component::capability::accelerometer::v1::Sample as AccelerometerData;
use phoxal::api::component::capability::battery::v1::State as BatteryData;
use phoxal::api::component::capability::camera::v1::Frame as CameraData;
use phoxal::api::component::capability::depth::v1::Depth as DepthData;
use phoxal::api::component::capability::encoder::v1::Sample as EncoderData;
use phoxal::api::component::capability::gnss::v1::Sample as GnssData;
use phoxal::api::component::capability::gyroscope::v1::Sample as GyroscopeData;
use phoxal::api::component::capability::imu::v1::Sample as ImuData;
use phoxal::api::component::capability::lidar::v1::Scan as LidarData;
use phoxal::api::component::capability::magnetometer::v1::Sample as MagnetometerData;
use phoxal::api::component::capability::microphone::v1::Frame as MicrophoneData;
use phoxal::api::component::capability::mmwave::v1::Scan as MmwaveData;
use phoxal::api::component::capability::profile::v1::{
    ParsedCameraProfileSpec, ParsedDepthProfileSpec, ProfileId,
};
use phoxal::api::component::capability::range::v1::Sample as RangeData;
use phoxal::api::topic;
use phoxal::bus::Bus;
use phoxal::bus::liveliness::{LivelinessEvent, liveliness_subscriber};
use phoxal::bus::topic::{ANY, PubSub, Topic};
use phoxal::model::component::v1::CapabilityRef;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{debug, trace, warn};
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::pubsub::Publisher as ZenohPublisher;

const LATEST_FRAME_QUEUE_CAPACITY: usize = 1;
const TELEMETRY_QUEUE_CAPACITY: usize = 64;
const MATCHING_POLL_PERIOD: Duration = Duration::from_millis(20);

type DemandedCapabilities = Arc<Mutex<BTreeSet<CapabilityRef>>>;
type RequestedProfiles = Arc<Mutex<BTreeMap<CapabilityRef, BTreeSet<ProfileId>>>>;
type ProfileWorkers = Arc<Mutex<BTreeMap<ProfileOutputKey, ProfileWorker>>>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProfileOutputKey {
    capability: CapabilityRef,
    profile_id: ProfileId,
}

impl ProfileOutputKey {
    fn new(capability: CapabilityRef, profile_id: ProfileId) -> Self {
        Self {
            capability,
            profile_id,
        }
    }
}

struct MatchingProbe {
    publisher: ZenohPublisher<'static>,
}

impl MatchingProbe {
    async fn new<T>(bus: &Bus, topic: &Topic<PubSub<T>>) -> Result<Self> {
        let key = topic.publish_key()?.into_owned();
        let key_expr =
            OwnedKeyExpr::new(bus.topic(&key)).map_err(|error| anyhow!(error.to_string()))?;
        let publisher = bus
            .session()
            .declare_publisher(key_expr)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok(Self { publisher })
    }

    async fn has_matching_subscribers(&self) -> Result<bool> {
        Ok(self
            .publisher
            .matching_status()
            .await
            .map_err(|error| anyhow!(error.to_string()))?
            .matching())
    }
}

macro_rules! output_capabilities {
    ($($variant:ident => { payload: $payload:ty, contract: $contract:ident, leaf: $leaf:ident, topic: $topic:expr }),+ $(,)?) => {
        #[derive(Debug, Clone)]
        pub enum Publish {
            $(
                $variant {
                    capability: CapabilityRef,
                    profile_id: ProfileId,
                    at_ns: u64,
                    payload: $payload,
                },
            )+
        }

        impl Publish {
            pub fn capability(&self) -> &CapabilityRef {
                match self {
                    $(
                        Self::$variant { capability, .. } => capability,
                    )+
                }
            }

            pub fn profile_id(&self) -> &ProfileId {
                match self {
                    $(
                        Self::$variant { profile_id, .. } => profile_id,
                    )+
                }
            }

            pub const fn kind(&self) -> &'static str {
                match self {
                    $(
                        Self::$variant { .. } => phoxal::api::component::capability::$leaf::v1::KIND,
                    )+
                }
            }
        }

        enum CapabilityPublisher {
            $($variant {
                bus: Bus,
                topic: Topic<PubSub<phoxal::api::component::capability::$leaf::$contract>>,
                matching_probe: Option<MatchingProbe>,
            },)+
        }

        struct Publishers {
            publishers: BTreeMap<CapabilityRef, CapabilityPublisher>,
        }

        impl Publishers {
            async fn new(bus: &Bus, contract: &ControllerContract) -> Result<Self> {
                let mut publishers = BTreeMap::new();

                for component in &contract.components {
                    for capability in &component.capabilities {
                        let Some(publisher) =
                            Self::publisher_for(bus, &capability.controller, &capability.capability).await?
                        else {
                            continue;
                        };

                        let previous = publishers.insert(capability.capability.clone(), publisher);
                        if previous.is_some() {
                            return Err(anyhow!(
                                "duplicate publisher registration for capability '{}'",
                                capability.capability
                            ));
                        }
                    }
                }

                Ok(Self { publishers })
            }

            async fn publisher_for(
                bus: &Bus,
                controller: &Controller,
                capability: &CapabilityRef,
            ) -> Result<Option<CapabilityPublisher>> {
                Self::publisher_for_profile(
                    bus,
                    controller,
                    capability,
                    &ProfileId::default_profile(),
                )
                .await
            }

            async fn publisher_for_profile(
                bus: &Bus,
                controller: &Controller,
                capability: &CapabilityRef,
                profile_id: &ProfileId,
            ) -> Result<Option<CapabilityPublisher>> {
                Ok(match controller {
                    $(
                        Controller::$variant(_) => {
                            let topic = $topic(capability, profile_id);
                            let matching_probe = if profile_id.as_ref() == ProfileId::DEFAULT
                                && demand_tracked_kind(phoxal::api::component::capability::$leaf::v1::KIND)
                            {
                                Some(MatchingProbe::new(bus, &topic).await?)
                            } else {
                                None
                            };
                            Some(CapabilityPublisher::$variant {
                                bus: bus.clone(),
                                topic,
                                matching_probe,
                            })
                        }
                    )+
                    Controller::Motor(_) | Controller::Led(_) | Controller::Speaker(_) => None,
                })
            }

            fn into_publishers(self) -> BTreeMap<CapabilityRef, CapabilityPublisher> {
                self.publishers
            }
        }

        impl CapabilityPublisher {
            const fn kind(&self) -> &'static str {
                match self {
                    $(
                        Self::$variant { .. } => phoxal::api::component::capability::$leaf::v1::KIND,
                    )+
                }
            }

            async fn has_matching_subscribers(&self) -> Result<bool> {
                match self {
                    $(
                        Self::$variant { matching_probe, .. } => {
                            match matching_probe {
                                Some(matching_probe) => matching_probe.has_matching_subscribers().await,
                                None => Ok(true),
                            }
                        }
                    )+
                }
            }

            async fn publish(&self, publish: Publish) -> Result<()> {
                match publish {
                    $(
                        Publish::$variant { capability, at_ns, payload, .. } => {
                            match self {
                                CapabilityPublisher::$variant { bus, topic, .. } => {
                                    let payload =
                                        phoxal::api::component::capability::$leaf::$contract::V1(payload);
                                    bus.publish(topic, at_ns, &payload).await?;
                                }
                                _ => {
                                    return Err(anyhow!(
                                        "publisher type mismatch for capability '{}'",
                                        capability
                                    ));
                                }
                            }
                        }
                    )+
                }

                Ok(())
            }
        }
    };
}

output_capabilities! {
    Encoder => {
        payload: EncoderData,
        contract: Sample,
        leaf: encoder,
        topic: |capability: &CapabilityRef, _profile_id: &ProfileId| {
            topic::new().component(&capability.component_id).encoder(&capability.capability_id).data()
        }
    },
    Accelerometer => {
        payload: AccelerometerData,
        contract: Sample,
        leaf: accelerometer,
        topic: |capability: &CapabilityRef, _profile_id: &ProfileId| {
            topic::new().component(&capability.component_id).accelerometer(&capability.capability_id).data()
        }
    },
    Battery => {
        payload: BatteryData,
        contract: State,
        leaf: battery,
        topic: |capability: &CapabilityRef, _profile_id: &ProfileId| {
            topic::new().component(&capability.component_id).battery(&capability.capability_id).data()
        }
    },
    Camera => {
        payload: CameraData,
        contract: Frame,
        leaf: camera,
        topic: |capability: &CapabilityRef, profile_id: &ProfileId| {
            if profile_id.as_ref() == ProfileId::DEFAULT {
                topic::new().component(&capability.component_id).camera(&capability.capability_id).data()
            } else {
                topic::new()

                    .component(&capability.component_id)
                    .camera(&capability.capability_id)
                    .profile(profile_id.as_ref())
                    .data()
            }
        }
    },
    Depth => {
        payload: DepthData,
        contract: Depth,
        leaf: depth,
        topic: |capability: &CapabilityRef, profile_id: &ProfileId| {
            if profile_id.as_ref() == ProfileId::DEFAULT {
                topic::new().component(&capability.component_id).depth(&capability.capability_id).data()
            } else {
                topic::new()

                    .component(&capability.component_id)
                    .depth(&capability.capability_id)
                    .profile(profile_id.as_ref())
                    .data()
            }
        }
    },
    Range => {
        payload: RangeData,
        contract: Sample,
        leaf: range,
        topic: |capability: &CapabilityRef, _profile_id: &ProfileId| {
            topic::new().component(&capability.component_id).range(&capability.capability_id).data()
        }
    },
    Gnss => {
        payload: GnssData,
        contract: Sample,
        leaf: gnss,
        topic: |capability: &CapabilityRef, _profile_id: &ProfileId| {
            topic::new().component(&capability.component_id).gnss(&capability.capability_id).data()
        }
    },
    Gyroscope => {
        payload: GyroscopeData,
        contract: Sample,
        leaf: gyroscope,
        topic: |capability: &CapabilityRef, _profile_id: &ProfileId| {
            topic::new().component(&capability.component_id).gyroscope(&capability.capability_id).data()
        }
    },
    Imu => {
        payload: ImuData,
        contract: Sample,
        leaf: imu,
        topic: |capability: &CapabilityRef, _profile_id: &ProfileId| {
            topic::new().component(&capability.component_id).imu(&capability.capability_id).data()
        }
    },
    Lidar => {
        payload: LidarData,
        contract: Scan,
        leaf: lidar,
        topic: |capability: &CapabilityRef, _profile_id: &ProfileId| {
            topic::new().component(&capability.component_id).lidar(&capability.capability_id).data()
        }
    },
    Magnetometer => {
        payload: MagnetometerData,
        contract: Sample,
        leaf: magnetometer,
        topic: |capability: &CapabilityRef, _profile_id: &ProfileId| {
            topic::new().component(&capability.component_id).magnetometer(&capability.capability_id).data()
        }
    },
    Microphone => {
        payload: MicrophoneData,
        contract: Frame,
        leaf: microphone,
        topic: |capability: &CapabilityRef, _profile_id: &ProfileId| {
            topic::new().component(&capability.component_id).microphone(&capability.capability_id).data()
        }
    },
    Mmwave => {
        payload: MmwaveData,
        contract: Scan,
        leaf: mmwave,
        topic: |capability: &CapabilityRef, _profile_id: &ProfileId| {
            topic::new().component(&capability.component_id).mmwave(&capability.capability_id).data()
        }
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueuePolicy {
    LatestFrameWins,
    DropOldest,
}

impl QueuePolicy {
    fn for_kind(kind: &str) -> Self {
        match kind {
            phoxal::api::component::capability::camera::v1::KIND
            | phoxal::api::component::capability::depth::v1::KIND => Self::LatestFrameWins,
            _ => Self::DropOldest,
        }
    }

    const fn capacity(self) -> usize {
        match self {
            Self::LatestFrameWins => LATEST_FRAME_QUEUE_CAPACITY,
            Self::DropOldest => TELEMETRY_QUEUE_CAPACITY,
        }
    }
}

#[derive(Default)]
struct QueueState {
    pending: VecDeque<Publish>,
    captured_count: u64,
    dropped_count: u64,
}

struct OutputQueue {
    state: Mutex<QueueState>,
    notify: Notify,
    capacity: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnqueueReport {
    captured_count: u64,
    dropped_count: u64,
    dropped: bool,
}

impl OutputQueue {
    fn new(policy: QueuePolicy) -> Self {
        Self {
            state: Mutex::new(QueueState::default()),
            notify: Notify::new(),
            capacity: policy.capacity(),
        }
    }

    fn push(&self, output: Publish) -> EnqueueReport {
        let mut state = self
            .state
            .lock()
            .expect("Webots output queue mutex should not be poisoned");
        state.captured_count = state.captured_count.saturating_add(1);
        let dropped = if state.pending.len() >= self.capacity {
            state.pending.pop_front();
            state.dropped_count = state.dropped_count.saturating_add(1);
            true
        } else {
            false
        };
        state.pending.push_back(output);
        let report = EnqueueReport {
            captured_count: state.captured_count,
            dropped_count: state.dropped_count,
            dropped,
        };
        drop(state);
        self.notify.notify_one();
        report
    }

    async fn recv(&self) -> Publish {
        loop {
            let notified = self.notify.notified();
            if let Some(output) = self
                .state
                .lock()
                .expect("Webots output queue mutex should not be poisoned")
                .pending
                .pop_front()
            {
                return output;
            }
            notified.await;
        }
    }
}

struct OutputWorker {
    capability: CapabilityRef,
    kind: &'static str,
    queue: Arc<OutputQueue>,
}

impl OutputWorker {
    fn enqueue(&self, output: Publish) {
        let report = self.queue.push(output);
        if report.dropped {
            debug!(
                capability = %self.capability,
                capability_kind = self.kind,
                captured_count = report.captured_count,
                dropped_count = report.dropped_count,
                "dropped queued Webots output before publication"
            );
        }
    }
}

struct ProfileWorker {
    capability: CapabilityRef,
    profile_id: ProfileId,
    kind: &'static str,
    queue: Arc<OutputQueue>,
    handle: JoinHandle<()>,
}

impl ProfileWorker {
    fn enqueue(&self, output: Publish) {
        let report = self.queue.push(output);
        if report.dropped {
            debug!(
                capability = %self.capability,
                profile_id = %self.profile_id,
                capability_kind = self.kind,
                captured_count = report.captured_count,
                dropped_count = report.dropped_count,
                "dropped queued Webots profile output before publication"
            );
        }
    }
}

impl Drop for ProfileWorker {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

pub struct OutputDispatcher {
    workers: BTreeMap<CapabilityRef, OutputWorker>,
    demanded_capabilities: DemandedCapabilities,
    requested_profiles: RequestedProfiles,
    profile_workers: ProfileWorkers,
    _worker_handles: Vec<JoinHandle<()>>,
}

impl OutputDispatcher {
    pub async fn new(bus: &Bus, contract: &ControllerContract) -> Result<Self> {
        Ok(Self::from_publishers(
            bus,
            contract,
            Publishers::new(bus, contract).await?,
        ))
    }

    fn from_publishers(bus: &Bus, contract: &ControllerContract, publishers: Publishers) -> Self {
        let mut workers = BTreeMap::new();
        let mut worker_handles = Vec::new();
        let demanded_capabilities = Arc::new(Mutex::new(BTreeSet::new()));
        let requested_profiles = Arc::new(Mutex::new(BTreeMap::new()));
        let profile_workers = Arc::new(Mutex::new(BTreeMap::new()));

        for (capability, publisher) in publishers.into_publishers() {
            let kind = publisher.kind();
            let queue = Arc::new(OutputQueue::new(QueuePolicy::for_kind(kind)));
            worker_handles.push(spawn_publisher_worker(
                capability.clone(),
                kind,
                queue.clone(),
                publisher,
                demanded_capabilities.clone(),
            ));
            workers.insert(
                capability.clone(),
                OutputWorker {
                    capability,
                    kind,
                    queue,
                },
            );
        }

        for component in &contract.components {
            for capability in &component.capabilities {
                if !profile_demand_tracked_controller(&capability.controller) {
                    continue;
                }
                worker_handles.push(spawn_profile_demand_worker(
                    bus.clone(),
                    capability.capability.clone(),
                    capability.controller.clone(),
                    requested_profiles.clone(),
                    profile_workers.clone(),
                ));
            }
        }

        Self {
            workers,
            demanded_capabilities,
            requested_profiles,
            profile_workers,
            _worker_handles: worker_handles,
        }
    }

    pub fn demanded_capabilities(&self) -> BTreeSet<CapabilityRef> {
        self.demanded_capabilities
            .lock()
            .expect("Webots output demand mutex should not be poisoned")
            .clone()
    }

    pub fn requested_profiles(&self) -> BTreeMap<CapabilityRef, BTreeSet<ProfileId>> {
        self.requested_profiles
            .lock()
            .expect("Webots profile demand mutex should not be poisoned")
            .clone()
    }

    pub fn enqueue(&self, outputs: Vec<Publish>) {
        for output in outputs {
            let capability = output.capability().clone();
            if output.profile_id().as_ref() != ProfileId::DEFAULT {
                let key = ProfileOutputKey::new(capability.clone(), output.profile_id().clone());
                let profile_workers = self
                    .profile_workers
                    .lock()
                    .expect("Webots profile worker mutex should not be poisoned");
                let Some(worker) = profile_workers.get(&key) else {
                    warn!(
                        capability = %capability,
                        profile_id = %output.profile_id(),
                        capability_kind = output.kind(),
                        "dropped Webots profile output with no active publisher worker"
                    );
                    continue;
                };
                worker.enqueue(output);
                continue;
            }
            let kind = output.kind();
            let Some(worker) = self.workers.get(&capability) else {
                warn!(
                    capability = %capability,
                    capability_kind = kind,
                    "dropped Webots output with no publisher worker"
                );
                continue;
            };
            worker.enqueue(output);
        }
    }
}

fn spawn_publisher_worker(
    capability: CapabilityRef,
    kind: &'static str,
    queue: Arc<OutputQueue>,
    publisher: CapabilityPublisher,
    demanded_capabilities: DemandedCapabilities,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut published_count = 0_u64;
        let mut publish_error_count = 0_u64;

        if demand_tracked_kind(kind) {
            let mut matching_interval = tokio::time::interval(MATCHING_POLL_PERIOD);
            matching_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = matching_interval.tick() => {
                        match publisher.has_matching_subscribers().await {
                            Ok(matching) => update_demanded_capability(
                                &demanded_capabilities,
                                &capability,
                                kind,
                                matching,
                            ),
                            Err(error) => {
                                warn!(
                                    capability = %capability,
                                    capability_kind = kind,
                                    error = %error,
                                    "failed to read Webots output matching status"
                                );
                            }
                        }
                    }
                    output = queue.recv() => {
                        publish_output(
                            &publisher,
                            output,
                            &capability,
                            kind,
                            &mut published_count,
                            &mut publish_error_count,
                        ).await;
                    }
                }
            }
        }

        loop {
            let output = queue.recv().await;
            publish_output(
                &publisher,
                output,
                &capability,
                kind,
                &mut published_count,
                &mut publish_error_count,
            )
            .await;
        }
    })
}

async fn publish_output(
    publisher: &CapabilityPublisher,
    output: Publish,
    capability: &CapabilityRef,
    kind: &'static str,
    published_count: &mut u64,
    publish_error_count: &mut u64,
) {
    match publisher.publish(output).await {
        Ok(()) => {
            *published_count = published_count.saturating_add(1);
            trace!(
                capability = %capability,
                capability_kind = kind,
                published_count = *published_count,
                "published queued Webots output"
            );
        }
        Err(error) => {
            *publish_error_count = publish_error_count.saturating_add(1);
            warn!(
                capability = %capability,
                capability_kind = kind,
                publish_error_count = *publish_error_count,
                error = %error,
                "failed to publish queued Webots output"
            );
        }
    }
}

fn demand_tracked_kind(kind: &str) -> bool {
    kind == phoxal::api::component::capability::camera::v1::KIND
        || kind == phoxal::api::component::capability::depth::v1::KIND
}

fn update_demanded_capability(
    demanded_capabilities: &DemandedCapabilities,
    capability: &CapabilityRef,
    kind: &'static str,
    matching: bool,
) {
    let mut demanded = demanded_capabilities
        .lock()
        .expect("Webots output demand mutex should not be poisoned");
    let changed = if matching {
        demanded.insert(capability.clone())
    } else {
        demanded.remove(capability)
    };
    drop(demanded);

    if changed {
        trace!(
            capability = %capability,
            capability_kind = kind,
            matching,
            "updated demanded Webots output capability"
        );
    }
}

fn profile_demand_tracked_controller(controller: &Controller) -> bool {
    matches!(controller, Controller::Camera(_) | Controller::Depth(_))
}

fn spawn_profile_demand_worker(
    bus: Bus,
    capability: CapabilityRef,
    controller: Controller,
    requested_profiles: RequestedProfiles,
    profile_workers: ProfileWorkers,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Some(profile_prefix) = profile_liveliness_key(&capability, &controller) else {
            return;
        };
        let subscriber = match liveliness_subscriber(&bus, &profile_prefix).await {
            Ok(subscriber) => subscriber,
            Err(error) => {
                warn!(
                    capability = %capability,
                    capability_kind = controller.kind(),
                    error = %error,
                    "failed to subscribe to Webots profile liveliness"
                );
                return;
            }
        };

        loop {
            match subscriber.recv().await {
                Ok(LivelinessEvent::Alive(key)) => {
                    handle_profile_alive(
                        &bus,
                        &capability,
                        &controller,
                        &requested_profiles,
                        &profile_workers,
                        &key,
                    )
                    .await;
                }
                Ok(LivelinessEvent::Dropped(key)) => {
                    handle_profile_dropped(
                        &capability,
                        &controller,
                        &requested_profiles,
                        &profile_workers,
                        &key,
                    );
                }
                Err(error) => {
                    warn!(
                        capability = %capability,
                        capability_kind = controller.kind(),
                        error = %error,
                        "Webots profile liveliness subscription stopped"
                    );
                    return;
                }
            }
        }
    })
}

fn profile_liveliness_key(capability: &CapabilityRef, controller: &Controller) -> Option<String> {
    match controller {
        Controller::Camera(_) => Some(
            topic::new()
                .component(&capability.component_id)
                .camera(&capability.capability_id)
                .profile(ANY)
                .data()
                .key()
                .into_owned(),
        ),
        Controller::Depth(_) => Some(
            topic::new()
                .component(&capability.component_id)
                .depth(&capability.capability_id)
                .profile(ANY)
                .data()
                .key()
                .into_owned(),
        ),
        _ => None,
    }
}

async fn handle_profile_alive(
    bus: &Bus,
    capability: &CapabilityRef,
    controller: &Controller,
    requested_profiles: &RequestedProfiles,
    profile_workers: &ProfileWorkers,
    key: &str,
) {
    let Some(profile_id) = parsed_requested_profile_id(capability, controller, key) else {
        return;
    };
    let output_key = ProfileOutputKey::new(capability.clone(), profile_id.clone());
    if profile_workers
        .lock()
        .expect("Webots profile worker mutex should not be poisoned")
        .contains_key(&output_key)
    {
        return;
    }

    let Some(publisher) =
        (match Publishers::publisher_for_profile(bus, controller, capability, &profile_id).await {
            Ok(publisher) => publisher,
            Err(error) => {
                warn!(
                    capability = %capability,
                    profile_id = %profile_id,
                    capability_kind = controller.kind(),
                    error = %error,
                    "failed to create Webots requested-profile publisher"
                );
                return;
            }
        })
    else {
        return;
    };

    let kind = publisher.kind();
    let queue = Arc::new(OutputQueue::new(QueuePolicy::for_kind(kind)));
    let handle = spawn_profile_publisher_worker(
        capability.clone(),
        profile_id.clone(),
        kind,
        queue.clone(),
        publisher,
    );
    profile_workers
        .lock()
        .expect("Webots profile worker mutex should not be poisoned")
        .insert(
            output_key,
            ProfileWorker {
                capability: capability.clone(),
                profile_id: profile_id.clone(),
                kind,
                queue,
                handle,
            },
        );
    update_requested_profile(requested_profiles, capability, &profile_id, true, kind);
}

fn handle_profile_dropped(
    capability: &CapabilityRef,
    controller: &Controller,
    requested_profiles: &RequestedProfiles,
    profile_workers: &ProfileWorkers,
    key: &str,
) {
    let Some(profile_id) = parsed_requested_profile_id(capability, controller, key) else {
        return;
    };
    update_requested_profile(
        requested_profiles,
        capability,
        &profile_id,
        false,
        controller.kind(),
    );
    profile_workers
        .lock()
        .expect("Webots profile worker mutex should not be poisoned")
        .remove(&ProfileOutputKey::new(capability.clone(), profile_id));
}

fn parsed_requested_profile_id(
    capability: &CapabilityRef,
    controller: &Controller,
    key: &str,
) -> Option<ProfileId> {
    let Some((_, profile_suffix)) = key.rsplit_once("/profile/") else {
        warn!(
            capability = %capability,
            capability_kind = controller.kind(),
            key,
            "ignored Webots profile liveliness key without profile suffix"
        );
        return None;
    };
    let Some((profile_id, stream)) = profile_suffix.split_once('/') else {
        warn!(
            capability = %capability,
            capability_kind = controller.kind(),
            key,
            "ignored Webots profile liveliness key without profile data leaf"
        );
        return None;
    };
    if stream != "data" {
        return None;
    }
    if profile_id == ProfileId::DEFAULT {
        return None;
    }
    let profile_id = match ProfileId::new(profile_id) {
        Ok(profile_id) => profile_id,
        Err(error) => {
            warn!(
                capability = %capability,
                capability_kind = controller.kind(),
                key,
                error = %error,
                "ignored invalid Webots requested profile id"
            );
            return None;
        }
    };
    if let Err(error) = validate_profile_for_controller(controller, &profile_id) {
        warn!(
            capability = %capability,
            profile_id = %profile_id,
            capability_kind = controller.kind(),
            error = %error,
            "ignored Webots requested profile with wrong spec shape"
        );
        return None;
    }
    Some(profile_id)
}

fn validate_profile_for_controller(controller: &Controller, profile_id: &ProfileId) -> Result<()> {
    match controller {
        Controller::Camera(_) => {
            match phoxal::api::component::capability::profile::v1::CameraProfileSpec::from_profile_id(
                profile_id,
            )? {
                ParsedCameraProfileSpec::Native => {
                    anyhow::bail!("default profile is not a requested profile")
                }
                ParsedCameraProfileSpec::Spec(_) => Ok(()),
            }
        }
        Controller::Depth(_) => {
            match phoxal::api::component::capability::profile::v1::DepthProfileSpec::from_profile_id(
                profile_id,
            )? {
                ParsedDepthProfileSpec::Native => {
                    anyhow::bail!("default profile is not a requested profile")
                }
                ParsedDepthProfileSpec::Spec(_) => Ok(()),
            }
        }
        _ => anyhow::bail!("only camera and depth requested profiles are produced by this path"),
    }
}

fn update_requested_profile(
    requested_profiles: &RequestedProfiles,
    capability: &CapabilityRef,
    profile_id: &ProfileId,
    active: bool,
    kind: &'static str,
) {
    let mut requested = requested_profiles
        .lock()
        .expect("Webots profile demand mutex should not be poisoned");
    let changed = if active {
        requested
            .entry(capability.clone())
            .or_default()
            .insert(profile_id.clone())
    } else if let Some(profiles) = requested.get_mut(capability) {
        let changed = profiles.remove(profile_id);
        if profiles.is_empty() {
            requested.remove(capability);
        }
        changed
    } else {
        false
    };
    drop(requested);

    if changed {
        trace!(
            capability = %capability,
            profile_id = %profile_id,
            capability_kind = kind,
            active,
            "updated requested Webots output profile"
        );
    }
}

fn spawn_profile_publisher_worker(
    capability: CapabilityRef,
    profile_id: ProfileId,
    kind: &'static str,
    queue: Arc<OutputQueue>,
    publisher: CapabilityPublisher,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut published_count = 0_u64;
        let mut publish_error_count = 0_u64;
        loop {
            let output = queue.recv().await;
            publish_output(
                &publisher,
                output,
                &capability,
                kind,
                &mut published_count,
                &mut publish_error_count,
            )
            .await;
            trace!(
                capability = %capability,
                profile_id = %profile_id,
                capability_kind = kind,
                "processed queued Webots requested-profile output"
            );
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{OutputQueue, Publish, QueuePolicy, profile_liveliness_key};
    use crate::capabilities::camera::CameraMode;
    use crate::webots::controller::Controller;
    use phoxal::api::component::capability::camera::v1::{Encoding, Frame};
    use phoxal::api::component::capability::encoder::v1::Sample;
    use phoxal::api::component::capability::profile::v1::ProfileId;
    use phoxal::model::component::v1::CapabilityRef;

    fn camera_output(timestamp_ns: u64, value: u8) -> Publish {
        Publish::Camera {
            capability: CapabilityRef::new("front_camera", "rgb"),
            profile_id: ProfileId::default_profile(),
            at_ns: timestamp_ns,
            payload: Frame::new(1, 1, Encoding::L8, vec![value]),
        }
    }

    fn encoder_output(timestamp_ns: u64, ticks: i64) -> Publish {
        Publish::Encoder {
            capability: CapabilityRef::new("left_drive", "encoder"),
            profile_id: ProfileId::default_profile(),
            at_ns: timestamp_ns,
            payload: Sample::new(ticks),
        }
    }

    #[test]
    fn profile_liveliness_uses_profile_data_leaf_wildcard() {
        let camera = CapabilityRef::new("front_camera", "rgb");
        let camera_controller = Controller::Camera(crate::capabilities::camera::Config {
            mode: CameraMode::Rgb,
            publish_rate_hz: 30.0,
            sampling_period_hz: 30.0,
        });
        assert_eq!(
            profile_liveliness_key(&camera, &camera_controller).as_deref(),
            Some("component/front_camera/camera/rgb/profile/*/data")
        );
        let depth = CapabilityRef::new("front_camera", "depth");
        let depth_controller = Controller::Depth(crate::capabilities::depth::Config {
            publish_rate_hz: 30.0,
            sampling_period_hz: 30.0,
        });
        assert_eq!(
            profile_liveliness_key(&depth, &depth_controller).as_deref(),
            Some("component/front_camera/depth/depth/profile/*/data")
        );
    }

    #[tokio::test]
    async fn latest_frame_queue_replaces_pending_frame() {
        let queue = OutputQueue::new(QueuePolicy::LatestFrameWins);

        assert!(!queue.push(camera_output(1, 1)).dropped);
        let report = queue.push(camera_output(2, 2));

        assert!(report.dropped);
        assert_eq!(report.captured_count, 2);
        assert_eq!(report.dropped_count, 1);
        match queue.recv().await {
            Publish::Camera { at_ns, payload, .. } => {
                assert_eq!(at_ns, 2);
                assert_eq!(payload.data(), &[2]);
            }
            output => panic!("expected camera output, got {output:?}"),
        }
    }

    #[tokio::test]
    async fn telemetry_queue_drops_oldest_when_full() {
        let queue = OutputQueue::new(QueuePolicy::DropOldest);

        for index in 0..super::TELEMETRY_QUEUE_CAPACITY {
            assert!(
                !queue
                    .push(encoder_output(index as u64, index as i64))
                    .dropped
            );
        }
        let report = queue.push(encoder_output(999, 999));

        assert!(report.dropped);
        assert_eq!(
            report.captured_count,
            super::TELEMETRY_QUEUE_CAPACITY as u64 + 1
        );
        assert_eq!(report.dropped_count, 1);
        match queue.recv().await {
            Publish::Encoder { at_ns, payload, .. } => {
                assert_eq!(at_ns, 1);
                assert_eq!(payload.ticks(), 1);
            }
            output => panic!("expected encoder output, got {output:?}"),
        }
    }
}
