//! Runtime execution and observability helpers.
//!
//! Runtime decisions are logged through [`decision_log::DecisionLog`], not
//! runtime-local `last_logged_state` fields or free-text event names. The
//! runtime owns the typed decision key, normally derived from its owner-local
//! API `State` contract. `phoxal::runtime` owns the logging mechanics.
//!
//! Each runtime calls `observe(now_ns, key)` once per step with logical time
//! from [`clock::Step`]. The initial key always emits. Identical keys are
//! silent. Changes are emitted only when the key differs from the last emitted
//! key, and the helper bounds flapping with a logical-time `min_interval_ns`;
//! in-window transitions are folded into the next emitted event via
//! `suppressed_count`.
//!
//! All decision logs use one structured tracing event on target
//! `phoxal.runtime.decision` with message `runtime decision changed`. Every
//! event carries `runtime_id`, `decision_label`, `schema_name`,
//! `schema_version`, `decision_key`, `now_ns`, and `suppressed_count`.
//! Decision logging is observability only; it does not create a bus topic or
//! product.

pub mod clock;
pub mod conventions;
pub mod decision_log;
pub mod execute;
pub mod query;
pub mod sensor;

use std::any::Any;
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::api::presence::{Heartbeat, Readiness, RuntimeId};
use crate::api::v1::topic;
use crate::bus::Bus;
use crate::bus::builder::Builder;
use crate::bus::topic::{PubSub, Query as TopicQuery, Topic};
use crate::bus::typed::{Received, TypedTopicResponder, TypedTopicSubscriber};
use crate::bus::zenoh::BusyResponse;
use crate::model::structure::Structure;
use crate::runtime::clock::{Step, StepStream};
use crate::util::parse_trimmed_non_empty;
use anyhow::{Result, anyhow};
use clap::Parser;
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;

pub use conventions::*;
pub use execute::execute;
pub use query::{QueryOptions, ReadCell, Reader};

pub const ENV_ROBOT_CONFIG: &str = "ROBOT_CONFIG";
pub const ENV_ROBOT_ROUTER_ENDPOINT: &str = "ROBOT_ROUTER_ENDPOINT";
pub const ENV_ROBOT_SIMULATION: &str = "ROBOT_SIMULATION";
pub const ENV_ROBOT_CONNECT_TIMEOUT_MS: &str = "ROBOT_CONNECT_TIMEOUT_MS";
pub const ENV_ROBOT_CONNECT_RETRIES: &str = "ROBOT_CONNECT_RETRIES";
pub const ENV_COMPONENT_ID: &str = "COMPONENT_ID";
pub const ENV_ROBOT_ID: &str = "ROBOT_ID";
pub const ENV_ROBOT_NAMESPACE: &str = "ROBOT_NAMESPACE";

const DEFAULT_STALE_CYCLE_COUNT: f64 = 2.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RobotIdentity {
    pub robot_id: String,
    pub robot_namespace: String,
}

impl RobotIdentity {
    pub fn new(robot_id: impl Into<String>, robot_namespace: impl Into<String>) -> Self {
        Self {
            robot_id: robot_id.into(),
            robot_namespace: robot_namespace.into(),
        }
    }

    pub fn host_name(&self) -> String {
        format!("{}-{}", self.robot_namespace, self.robot_id)
    }
}

pub fn stale_timeout_ns(publish_hz: f64) -> u64 {
    ((DEFAULT_STALE_CYCLE_COUNT / publish_hz) * 1_000_000_000.0) as u64
}

/// Shared CLI arguments for all robot binaries.
#[derive(Debug, Parser, Clone)]
pub struct RobotRuntimeArgs {
    /// Path to a bundled robot directory containing robot.yaml, components/, and structure.urdf.
    #[arg(long, env = ENV_ROBOT_CONFIG)]
    pub robot_config: PathBuf,

    #[arg(long, env = ENV_ROBOT_ID, value_parser = parse_trimmed_non_empty)]
    pub robot_id: Option<String>,

    #[arg(
        long,
        env = ENV_ROBOT_NAMESPACE,
        default_value_t = String::from(conventions::DEFAULT_ROBOT_NAMESPACE),
        value_parser = parse_trimmed_non_empty
    )]
    pub robot_namespace: String,

    /// Zenoh router endpoint (for example, tcp/router:7447).
    #[arg(
        long = "robot-router-endpoint",
        env = ENV_ROBOT_ROUTER_ENDPOINT
    )]
    pub robot_router_endpoint: Option<String>,

    /// Consume the shared simulation clock instead of synthesizing a wall clock.
    #[arg(long, env = ENV_ROBOT_SIMULATION, default_value_t = false)]
    pub simulation: bool,

    /// Zenoh connect timeout in milliseconds.
    #[arg(
        long = "robot-connect-timeout-ms",
        env = ENV_ROBOT_CONNECT_TIMEOUT_MS,
        default_value_t = 60_000_u64
    )]
    pub robot_connect_timeout_ms: u64,

    /// Zenoh connection retries after the initial attempt.
    #[arg(
        long = "robot-connect-retries",
        env = ENV_ROBOT_CONNECT_RETRIES,
        default_value_t = 5_u32
    )]
    pub robot_connect_retries: u32,

    /// Hidden process ownership marker used by xtask local session cleanup.
    #[arg(long = "xtask-session", hide = true)]
    pub xtask_session: Option<String>,
}

#[derive(Debug, Parser, Clone)]
pub struct DriverRuntimeArgs {
    #[command(flatten)]
    pub runtime: RobotRuntimeArgs,

    /// Component instance identifier for this component driver service.
    #[arg(long = "component-id", env = ENV_COMPONENT_ID)]
    pub component_id: String,
}

impl RobotRuntimeArgs {
    pub fn identity(&self) -> RobotIdentity {
        RobotIdentity::from(self)
    }

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.robot_connect_timeout_ms)
    }

    pub fn robot(&self) -> Result<crate::model::v1::Robot> {
        crate::model::v1::Robot::read_from_dir(&self.robot_config)
    }

    pub fn resolved_facts(&self) -> Result<crate::model::robot::v1::ResolvedFacts> {
        self.robot()?.resolve()
    }

    pub fn structure(&self) -> Result<Structure> {
        Ok(self.robot()?.structure)
    }

    pub async fn connect_bus(&self) -> Result<Bus> {
        Builder::from(self).connect().await.map_err(Into::into)
    }
}

impl DriverRuntimeArgs {
    pub fn identity(&self) -> RobotIdentity {
        self.runtime.identity()
    }

    pub fn simulation(&self) -> bool {
        self.runtime.simulation
    }
}

impl From<&RobotRuntimeArgs> for Builder {
    fn from(args: &RobotRuntimeArgs) -> Self {
        Builder::new(
            args.robot_router_endpoint
                .clone()
                .unwrap_or_else(|| "tcp/router:7447".to_string()),
        )
        .with_connect_timeout(args.connect_timeout())
        .with_connect_retries(args.robot_connect_retries)
        .with_prefix(args.robot_namespace.clone())
    }
}

impl From<RobotRuntimeArgs> for Builder {
    fn from(args: RobotRuntimeArgs) -> Self {
        Self::from(&args)
    }
}

impl From<&RobotRuntimeArgs> for RobotIdentity {
    fn from(args: &RobotRuntimeArgs) -> Self {
        Self::new(
            args.robot_id.clone().unwrap_or_default(),
            args.robot_namespace.clone(),
        )
    }
}

impl From<&DriverRuntimeArgs> for RobotIdentity {
    fn from(args: &DriverRuntimeArgs) -> Self {
        Self::from(&args.runtime)
    }
}

// Empty per-runtime CLI extension for runtimes that only use common flags.
#[derive(Debug, Clone, Copy, clap::Args)]
pub struct EmptyArgs;

#[derive(
    Debug, Copy, Clone, Eq, PartialEq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    P0,
    P1,
    P2,
    P3,
    P4,
    P5,
}

impl Phase {
    pub const ALL: &'static [Phase] = &[
        Phase::P0,
        Phase::P1,
        Phase::P2,
        Phase::P3,
        Phase::P4,
        Phase::P5,
    ];

    pub const fn slug(self) -> &'static str {
        match self {
            Self::P0 => "p0",
            Self::P1 => "p1",
            Self::P2 => "p2",
            Self::P3 => "p3",
            Self::P4 => "p4",
            Self::P5 => "p5",
        }
    }
}

/// Static descriptor used by `Runtime::scenarios()` and the
/// `scenarios list` subcommand.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ScenarioDescriptor {
    pub name: Cow<'static, str>,
    pub summary: Cow<'static, str>,

    /// Execution mode: in-process headless or Webots-backed live bus.
    pub kind: ScenarioKind,

    /// Delivery phase this scenario contributes to.
    pub phase: Phase,

    /// Wallclock budget. Orchestrator kills the scenario after this.
    pub timeout_secs: u64,

    /// Logical category for grouping in reports.
    pub category: Cow<'static, str>,

    /// Validation tier (1 = lifted in-process, 2 = Webots integration,
    /// 3 = robot acceptance, etc.).
    pub tier: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
pub enum ScenarioKind {
    /// In-process scenario; no live bus. Orchestrator passes --robot-config
    /// and spawns the owning runtime binary.
    Headless,

    /// Live-bus scenario requiring a Webots session running the named world.
    Webots { world: Cow<'static, str> },
}

#[async_trait::async_trait]
pub trait Runtime: Sized + Send {
    /// Stable identifier this runtime reports to the presence/liveness monitor.
    /// Must match the runtime's docker-compose service name (e.g. "map", "follow").
    const RUNTIME_ID: &'static str;

    /// Per-runtime CLI extension. Use `EmptyArgs` when no extra flags are needed.
    type Args: clap::Args + Send + Sync;
    type Config: Send;
    type Input: Send + 'static;

    /// Resolve typed runtime config from runtime-specific and framework-common args.
    fn config(args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config>;

    /// Clock period the runtime's step loop drives at.
    fn clock_period(config: &Self::Config) -> Duration;

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self>;

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()>;

    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }

    /// Scenario catalog this runtime owns. Default: empty.
    fn scenarios() -> &'static [ScenarioDescriptor] {
        &[]
    }

    /// Run a scenario by name. Default: bail out with a clear error.
    async fn run_scenario(
        name: &str,
        _common: &RobotRuntimeArgs,
        _args: &Self::Args,
    ) -> Result<()> {
        anyhow::bail!(
            "runtime '{}' has no scenarios registered; cannot run scenario '{}'",
            Self::RUNTIME_ID,
            name
        )
    }
}

/// How a single input source buffers messages between logical steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPolicy {
    /// Keep every message since the previous step, in arrival order.
    All,
    /// Keep only the most recent message; older unconsumed messages are dropped.
    Latest,
    /// Keep at most `max` messages; when full, drop the oldest.
    BoundedDropOldest { max: usize },
}

impl InputPolicy {
    pub const fn all() -> Self {
        Self::All
    }

    pub const fn latest() -> Self {
        Self::Latest
    }

    pub const fn bounded_drop_oldest(max: usize) -> Self {
        Self::BoundedDropOldest { max }
    }
}

/// Per-step aggregate input accounting. `received == delivered + dropped`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeInputStats {
    pub received: u64,
    pub delivered: u64,
    pub dropped: u64,
}

/// Inputs delivered to a runtime for one logical step, plus accounting.
pub struct RuntimeInputs<I> {
    events: Vec<I>,
    stats: RuntimeInputStats,
}

impl<I> RuntimeInputs<I> {
    pub fn stats(&self) -> RuntimeInputStats {
        self.stats
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, I> {
        self.events.iter()
    }
}

impl<I> IntoIterator for RuntimeInputs<I> {
    type Item = I;
    type IntoIter = std::vec::IntoIter<I>;

    fn into_iter(self) -> Self::IntoIter {
        self.events.into_iter()
    }
}

impl<I> Default for RuntimeInputs<I> {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            stats: RuntimeInputStats::default(),
        }
    }
}

impl<I> From<Vec<I>> for RuntimeInputs<I> {
    fn from(events: Vec<I>) -> Self {
        let len = events.len() as u64;
        Self {
            events,
            stats: RuntimeInputStats {
                received: len,
                delivered: len,
                dropped: 0,
            },
        }
    }
}

struct SourceBuffer<I> {
    policy: InputPolicy,
    queue: VecDeque<I>,
    received: u64,
    dropped: u64,
}

impl<I> SourceBuffer<I> {
    fn new(policy: InputPolicy) -> Self {
        Self {
            policy,
            queue: VecDeque::new(),
            received: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, item: I) {
        self.received = self.received.saturating_add(1);
        match self.policy {
            InputPolicy::All => {
                self.queue.push_back(item);
            }
            InputPolicy::Latest => {
                if !self.queue.is_empty() {
                    self.dropped = self.dropped.saturating_add(self.queue.len() as u64);
                    self.queue.clear();
                }
                self.queue.push_back(item);
            }
            InputPolicy::BoundedDropOldest { max } => {
                if max == 0 {
                    self.dropped = self.dropped.saturating_add(1);
                    return;
                }
                while self.queue.len() >= max {
                    self.queue.pop_front();
                    self.dropped = self.dropped.saturating_add(1);
                }
                self.queue.push_back(item);
            }
        }
    }

    fn drain_into(&mut self, out: &mut Vec<I>, stats: &mut RuntimeInputStats) {
        let delivered = self.queue.len() as u64;
        stats.received = stats.received.saturating_add(self.received);
        stats.delivered = stats.delivered.saturating_add(delivered);
        stats.dropped = stats.dropped.saturating_add(self.dropped);
        out.extend(self.queue.drain(..));
        self.received = 0;
        self.dropped = 0;
    }
}

type SourceHandle<I> = Arc<Mutex<SourceBuffer<I>>>;

trait RecordingBuffer: Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

struct TypedRecordingBuffer<T> {
    values: Arc<Mutex<Vec<T>>>,
}

impl<T: Send + 'static> RecordingBuffer for TypedRecordingBuffer<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct Io<Input> {
    bus: Option<Bus>,
    handles: Vec<JoinHandle<()>>,
    sources: Vec<SourceHandle<Input>>,
    recorded_puts: HashMap<String, Arc<dyn RecordingBuffer>>,
}

impl<Input> Io<Input> {
    pub fn recording() -> Self {
        Self {
            bus: None,
            handles: Vec::new(),
            sources: Vec::new(),
            recorded_puts: HashMap::new(),
        }
    }

    pub fn recorded_puts<T: Clone + 'static>(&self, topic: &str) -> Vec<T> {
        self.recorded_puts
            .get(topic)
            .and_then(|buffer| buffer.as_any().downcast_ref::<TypedRecordingBuffer<T>>())
            .and_then(|buffer| buffer.values.lock().ok().map(|values| values.clone()))
            .unwrap_or_default()
    }
}

impl<Input: Send + 'static> Io<Input> {
    fn live(bus: Bus, _runtime_id: &'static str) -> Self {
        Self {
            bus: Some(bus),
            handles: Vec::new(),
            sources: Vec::new(),
            recorded_puts: HashMap::new(),
        }
    }

    fn into_parts(self) -> (Vec<JoinHandle<()>>, Vec<SourceHandle<Input>>) {
        (self.handles, self.sources)
    }

    pub fn bus(&self) -> Result<Bus> {
        self.bus
            .clone()
            .ok_or_else(|| anyhow!("live bus is unavailable in recording IO"))
    }

    pub async fn subscribe_topic<T, F>(&mut self, topic: Topic<PubSub<T>>, map: F) -> Result<()>
    where
        T: DeserializeOwned + Send + Sync + 'static,
        F: Fn(Received<T>) -> Input + Send + 'static,
    {
        self.subscribe_topic_with(topic, InputPolicy::All, map)
            .await
    }

    pub async fn subscribe_topic_with<T, F>(
        &mut self,
        topic: Topic<PubSub<T>>,
        policy: InputPolicy,
        map: F,
    ) -> Result<()>
    where
        T: DeserializeOwned + Send + Sync + 'static,
        F: Fn(Received<T>) -> Input + Send + 'static,
    {
        if let Some(bus) = &self.bus {
            let source = Arc::new(Mutex::new(SourceBuffer::new(policy)));
            self.sources.push(source.clone());
            self.handles.push(spawn_topic_subscription_forwarder(
                bus.subscriber(&topic).await?,
                source,
                map,
            ));
        }
        Ok(())
    }

    pub async fn serve_query_topic<Req, Resp, V, F>(
        &mut self,
        topic: Topic<TopicQuery<Req, Resp>>,
        reader: Reader<V>,
        options: QueryOptions,
        handler: F,
    ) -> Result<()>
    where
        Req: DeserializeOwned + Send + Sync + 'static,
        Resp: Serialize + BusyResponse + Send + Sync + 'static,
        V: Send + Sync + 'static,
        F: Fn(&V, Req) -> Resp + Send + Sync + 'static,
    {
        if let Some(bus) = &self.bus {
            self.handles.push(spawn_topic_query_responder(
                bus.responder(&topic).await?,
                reader,
                options,
                handler,
            ));
        }
        Ok(())
    }

    pub async fn publisher_topic<T>(&mut self, topic: Topic<PubSub<T>>) -> Result<TopicPublisher<T>>
    where
        T: Serialize + Clone + Send + 'static,
    {
        let key = topic.publish_key()?.into_owned();

        if let Some(bus) = &self.bus {
            return Ok(TopicPublisher {
                inner: TopicPublisherInner::Live {
                    bus: bus.clone(),
                    topic,
                },
            });
        }

        let values = Arc::new(Mutex::new(Vec::new()));
        self.recorded_puts.insert(
            key,
            Arc::new(TypedRecordingBuffer {
                values: values.clone(),
            }),
        );
        Ok(TopicPublisher {
            inner: TopicPublisherInner::Recording(values),
        })
    }
}

pub struct TopicPublisher<T>
where
    T: Serialize + Clone,
{
    inner: TopicPublisherInner<T>,
}

enum TopicPublisherInner<T>
where
    T: Serialize + Clone,
{
    Live { bus: Bus, topic: Topic<PubSub<T>> },
    Recording(Arc<Mutex<Vec<T>>>),
}

impl<T> TopicPublisher<T>
where
    T: Serialize + Clone,
{
    pub async fn put(&self, at_ns: u64, data: &T) -> Result<()> {
        match &self.inner {
            TopicPublisherInner::Live { bus, topic } => {
                bus.publish(topic, at_ns, data).await.map_err(Into::into)
            }
            TopicPublisherInner::Recording(values) => {
                values
                    .lock()
                    .map_err(|error| anyhow!("recorded publisher lock poisoned: {error}"))?
                    .push(data.clone());
                Ok(())
            }
        }
    }
}

pub struct RuntimeProcess<'a> {
    bus: &'a Bus,
    simulation: bool,
    period: Duration,
}

impl<'a> RuntimeProcess<'a> {
    pub const fn new(bus: &'a Bus, simulation: bool, period: Duration) -> Self {
        Self {
            bus,
            simulation,
            period,
        }
    }

    pub async fn run<R>(self, config: R::Config) -> Result<()>
    where
        R: Runtime,
    {
        tracing::info!(
            runtime = R::RUNTIME_ID,
            simulation = self.simulation,
            period_ms = self.period.as_millis() as u64,
            "runtime starting"
        );

        let mut io = Io::live((*self.bus).clone(), R::RUNTIME_ID);
        let mut runtime = R::new(&mut io, config).await?;
        let mut steps = StepStream::new(self.bus, self.simulation, self.period).await?;
        let heartbeat_pub = io
            .publisher_topic(topic::new().v1().presence().heartbeat())
            .await?;
        let (handles, sources) = io.into_parts();
        let _handles = handles;

        tracing::info!(runtime = R::RUNTIME_ID, "runtime ready");

        let mut last_heartbeat_ns = None;
        let mut steps_in_window = 0_u64;
        let mut inputs_in_window = 0_u64;
        let mut dropped_in_window = 0_u64;

        loop {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    result?;
                    tracing::info!(runtime = R::RUNTIME_ID, "runtime shutting down");
                    runtime.shutdown().await?;
                    break;
                }
                result = steps.next() => {
                    let step = result?;
                    let inputs = collect_step_inputs(&sources)?;
                    let stats = inputs.stats();
                    runtime.step(step, inputs).await?;
                    let step_time_ns = step.tick.time_ns();
                    let window_start_ns = last_heartbeat_ns.get_or_insert(step_time_ns);
                    steps_in_window = steps_in_window.saturating_add(1);
                    inputs_in_window = inputs_in_window.saturating_add(stats.delivered);
                    dropped_in_window = dropped_in_window.saturating_add(stats.dropped);
                    let elapsed_ns = step_time_ns.saturating_sub(*window_start_ns);
                    if elapsed_ns >= 10_000_000_000 {
                        tracing::info!(
                            runtime = R::RUNTIME_ID,
                            steps = steps_in_window,
                            inputs = inputs_in_window,
                            dropped = dropped_in_window,
                            window_s = elapsed_ns as f64 / 1_000_000_000_f64,
                            "runtime alive"
                        );
                        last_heartbeat_ns = Some(step_time_ns);
                        steps_in_window = 0;
                        inputs_in_window = 0;
                        dropped_in_window = 0;
                    }
                    heartbeat_pub
                        .put(
                            step.tick.time_ns(),
                            &Heartbeat {
                                runtime_id: RuntimeId::new(R::RUNTIME_ID),
                                readiness: Readiness::Ready,
                            },
                        )
                        .await?;
                }
            }
        }

        Ok(())
    }
}

fn collect_step_inputs<I>(sources: &[SourceHandle<I>]) -> Result<RuntimeInputs<I>> {
    let mut events = Vec::new();
    let mut stats = RuntimeInputStats::default();
    for source in sources {
        source
            .lock()
            .map_err(|error| anyhow!("runtime input source lock poisoned: {error}"))?
            .drain_into(&mut events, &mut stats);
    }
    Ok(RuntimeInputs { events, stats })
}

fn spawn_topic_subscription_forwarder<T, U, F>(
    subscriber: TypedTopicSubscriber<T>,
    source: SourceHandle<U>,
    map: F,
) -> JoinHandle<()>
where
    T: DeserializeOwned + Send + Sync + 'static,
    U: Send + 'static,
    F: Fn(Received<T>) -> U + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match subscriber.recv().await {
                Ok(received) => {
                    push_source_input(&source, map(received));
                }
                Err(crate::bus::Error::TypedDecode(error)) => {
                    tracing::warn!(
                        %error,
                        payload_type = std::any::type_name::<T>(),
                        "failed to decode typed topic payload"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        payload_type = std::any::type_name::<T>(),
                        "failed to receive typed topic payload"
                    );
                    return;
                }
            }
        }
    })
}

fn spawn_topic_query_responder<Req, Resp, V, F>(
    responder: TypedTopicResponder<Req, Resp>,
    reader: Reader<V>,
    options: QueryOptions,
    handler: F,
) -> JoinHandle<()>
where
    Req: DeserializeOwned + Send + Sync + 'static,
    Resp: Serialize + BusyResponse + Send + Sync + 'static,
    V: Send + Sync + 'static,
    F: Fn(&V, Req) -> Resp + Send + Sync + 'static,
{
    let executor = QueryExecutor::new(reader, options, handler);

    tokio::spawn(async move {
        loop {
            let query = match responder.recv().await {
                Ok(query) => query,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        request_type = std::any::type_name::<Req>(),
                        "failed to receive typed topic query"
                    );
                    return;
                }
            };
            let request = match query.request() {
                Ok(request) => request,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        request_type = std::any::type_name::<Req>(),
                        "failed to decode typed topic query payload"
                    );
                    continue;
                }
            };
            match executor.start::<Req, Resp>(request) {
                QueryStart::Busy(response) => {
                    if let Err(error) = query.reply(&response).await {
                        tracing::warn!(
                            %error,
                            response_type = std::any::type_name::<Resp>(),
                            "failed to reply with busy typed topic query response"
                        );
                    }
                }
                QueryStart::Accepted(call) => {
                    tokio::spawn(async move {
                        let response = call.run();
                        if let Err(error) = query.reply(&response).await {
                            tracing::warn!(
                                %error,
                                response_type = std::any::type_name::<Resp>(),
                                "failed to reply to typed topic query"
                            );
                        }
                    });
                }
            }
        }
    })
}

struct QueryExecutor<V, F> {
    reader: Reader<V>,
    permits: Arc<Semaphore>,
    handler: Arc<F>,
}

impl<V, F> QueryExecutor<V, F> {
    fn new(reader: Reader<V>, options: QueryOptions, handler: F) -> Self {
        Self {
            reader,
            permits: Arc::new(Semaphore::new(options.max_in_flight.get())),
            handler: Arc::new(handler),
        }
    }
}

impl<V, F> QueryExecutor<V, F>
where
    V: Send + Sync + 'static,
    F: Send + Sync + 'static,
{
    fn start<Req, Resp>(&self, request: Req) -> QueryStart<Req, Resp, V, F>
    where
        Resp: BusyResponse,
    {
        match self.permits.clone().try_acquire_owned() {
            Ok(permit) => QueryStart::Accepted(QueryCall {
                permit,
                view: self.reader.load(),
                handler: self.handler.clone(),
                request,
            }),
            Err(_) => QueryStart::Busy(Resp::busy()),
        }
    }
}

enum QueryStart<Req, Resp, V, F> {
    Busy(Resp),
    Accepted(QueryCall<Req, V, F>),
}

struct QueryCall<Req, V, F> {
    permit: OwnedSemaphorePermit,
    view: Arc<V>,
    handler: Arc<F>,
    request: Req,
}

impl<Req, V, F> QueryCall<Req, V, F>
where
    F: Send + Sync + 'static,
{
    fn run<Resp>(self) -> Resp
    where
        F: Fn(&V, Req) -> Resp,
    {
        let Self {
            permit,
            view,
            handler,
            request,
        } = self;
        let _permit = permit;
        handler(&view, request)
    }
}

fn push_source_input<I>(source: &SourceHandle<I>, input: I) {
    match source.lock() {
        Ok(mut source) => source.push(input),
        Err(error) => {
            tracing::warn!(%error, "runtime input source lock poisoned");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::clock::{Schedule, SchedulePolicy};
    use crate::runtime::{QueryOptions, ReadCell};

    use super::{InputPolicy, QueryExecutor, QueryStart, RuntimeInputStats, SourceBuffer};
    use crate::bus::zenoh::BusyResponse;
    use std::sync::{Arc, Condvar, Mutex};

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct QueryRequest {
        id: u8,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum QueryResponse {
        Value(u8),
        Busy,
    }

    impl BusyResponse for QueryResponse {
        fn busy() -> Self {
            Self::Busy
        }
    }

    #[test]
    fn query_executor_returns_busy_when_max_in_flight_is_saturated() {
        let view = ReadCell::new(());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let release_handler = release.clone();

        let executor = QueryExecutor::new(
            view.reader(),
            QueryOptions::single(),
            move |_: &(), request: QueryRequest| {
                if request.id == 1 {
                    started_tx.send(()).expect("test should receive start");
                    let (lock, cvar) = &*release_handler;
                    let mut released = lock.lock().expect("release lock should not be poisoned");
                    while !*released {
                        released = cvar
                            .wait(released)
                            .expect("release lock should not be poisoned");
                    }
                }
                QueryResponse::Value(request.id)
            },
        );

        let first_call = match executor.start::<QueryRequest, QueryResponse>(QueryRequest { id: 1 })
        {
            QueryStart::Accepted(call) => call,
            QueryStart::Busy(_) => panic!("first query should acquire the single permit"),
        };
        let first = std::thread::spawn(move || first_call.run::<QueryResponse>());

        started_rx
            .recv()
            .expect("first query handler should signal start");

        let second = executor.start::<QueryRequest, QueryResponse>(QueryRequest { id: 2 });

        assert!(matches!(second, QueryStart::Busy(QueryResponse::Busy)));

        {
            let (lock, cvar) = &*release;
            let mut released = lock.lock().expect("release lock should not be poisoned");
            *released = true;
            cvar.notify_one();
        }

        let first = first
            .join()
            .expect("first query handler thread should complete");
        assert_eq!(first, QueryResponse::Value(1));
    }

    #[test]
    fn all_source_policy_preserves_arrival_order_without_drops() {
        let mut source = SourceBuffer::new(InputPolicy::All);
        source.push(1);
        source.push(2);
        source.push(3);

        let mut out = Vec::new();
        let mut stats = RuntimeInputStats::default();
        source.drain_into(&mut out, &mut stats);

        assert_eq!(out, vec![1, 2, 3]);
        assert_eq!(
            stats,
            RuntimeInputStats {
                received: 3,
                delivered: 3,
                dropped: 0,
            }
        );
        assert_eq!(stats.received, stats.delivered + stats.dropped);
    }

    #[test]
    fn latest_source_policy_keeps_only_newest_item() {
        let mut source = SourceBuffer::new(InputPolicy::Latest);
        for item in 1..=5 {
            source.push(item);
        }

        let mut out = Vec::new();
        let mut stats = RuntimeInputStats::default();
        source.drain_into(&mut out, &mut stats);

        assert_eq!(out, vec![5]);
        assert_eq!(
            stats,
            RuntimeInputStats {
                received: 5,
                delivered: 1,
                dropped: 4,
            }
        );
        assert_eq!(stats.received, stats.delivered + stats.dropped);
    }

    #[test]
    fn bounded_drop_oldest_source_policy_keeps_newest_max_items() {
        let mut source = SourceBuffer::new(InputPolicy::BoundedDropOldest { max: 3 });
        for item in 1..=6 {
            source.push(item);
        }

        let mut out = Vec::new();
        let mut stats = RuntimeInputStats::default();
        source.drain_into(&mut out, &mut stats);

        assert_eq!(out, vec![4, 5, 6]);
        assert_eq!(
            stats,
            RuntimeInputStats {
                received: 6,
                delivered: 3,
                dropped: 3,
            }
        );
        assert_eq!(stats.received, stats.delivered + stats.dropped);
    }

    #[test]
    fn source_policy_counters_reset_after_drain() {
        let mut source = SourceBuffer::new(InputPolicy::BoundedDropOldest { max: 2 });
        source.push(1);
        source.push(2);
        source.push(3);

        let mut out = Vec::new();
        let mut stats = RuntimeInputStats::default();
        source.drain_into(&mut out, &mut stats);

        let mut second_out = Vec::new();
        let mut second_stats = RuntimeInputStats::default();
        source.drain_into(&mut second_out, &mut second_stats);

        assert_eq!(second_out, Vec::<i32>::new());
        assert_eq!(second_stats, RuntimeInputStats::default());
    }

    #[test]
    fn collapse_schedule_respects_one_hz_cadence() {
        let mut schedule = Schedule::from_publish_hz(1.0, SchedulePolicy::Collapse);

        for tick in 1..10 {
            assert_eq!(schedule.due_steps(tick * 100_000_000), 0);
        }

        assert_eq!(schedule.due_steps(1_000_000_000), 1);
        for tick in 11..20 {
            assert_eq!(schedule.due_steps(tick * 100_000_000), 0);
        }
        assert_eq!(schedule.due_steps(2_000_000_000), 1);
    }

    #[test]
    fn collapse_schedule_emits_once_after_missed_periods() {
        let mut schedule = Schedule::from_publish_hz(1.0, SchedulePolicy::Collapse);

        assert_eq!(schedule.due_steps(3_500_000_000), 1);
        assert_eq!(schedule.due_steps(3_900_000_000), 0);
        assert_eq!(schedule.due_steps(4_000_000_000), 1);
    }
}
