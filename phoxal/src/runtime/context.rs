//! Runtime contexts: `SetupContext` (IO construction), `StepContext` (logical
//! time per scheduled step), and `ShutdownContext`.

use std::marker::PhantomData;
use std::time::Duration;

use crate::api::ContractBody;
use crate::bus::{
    Bus, DEFAULT_QUERY_TIMEOUT, Latest, LogicalTime, PubSub, Publisher, Querier, Query, Subscriber,
    Topic,
};
use crate::runtime::spec::{Declares, RuntimeFields};

/// Default drop-oldest ring depth for a `Subscriber` built in `#[setup]`.
const DEFAULT_SUBSCRIBER_DEPTH: usize = 32;

/// The sole IO-construction point, handed to `#[setup]` (D18).
///
/// Builders are bound `B: ContractBody<Api = R::Api>` (one API version — D60) and
/// `R: Declares<B>` (declared family only — D44): a body from another API version
/// or an undeclared family is a compile error. Normal runtime code never opens
/// Zenoh directly; the runner opened the bus before `#[setup]`.
pub struct SetupContext<R: RuntimeFields> {
    bus: Bus,
    _runtime: PhantomData<fn() -> R>,
}

impl<R: RuntimeFields> SetupContext<R> {
    pub(crate) fn new(bus: Bus) -> Self {
        SetupContext {
            bus,
            _runtime: PhantomData,
        }
    }

    /// Build a publisher for a declared pub/sub contract of this API version.
    pub async fn publisher<B>(&self, topic: Topic<PubSub<B>>) -> crate::Result<Publisher<B>>
    where
        B: ContractBody<Api = R::Api>,
        R: Declares<B>,
    {
        Ok(Publisher::new(self.bus.clone(), &topic)?)
    }

    /// Begin building a subscription (`.latest()` or `.subscriber()`).
    pub fn subscribe<B>(&self, topic: Topic<PubSub<B>>) -> SubscribeBuilder<R, B>
    where
        B: ContractBody<Api = R::Api>,
        R: Declares<B>,
    {
        SubscribeBuilder {
            bus: self.bus.clone(),
            topic,
            depth: DEFAULT_SUBSCRIBER_DEPTH,
            _runtime: PhantomData,
        }
    }

    /// Build a querier for a declared query contract of this API version. Carries
    /// the Phoxal-pinned finite timeout (D31).
    pub async fn querier<Req, Resp>(
        &self,
        topic: Topic<Query<Req, Resp>>,
    ) -> crate::Result<Querier<Req, Resp>>
    where
        Req: ContractBody<Api = R::Api>,
        Resp: ContractBody<Api = R::Api>,
        R: Declares<Req> + Declares<Resp>,
    {
        Ok(Querier::new(
            self.bus.clone(),
            &topic,
            DEFAULT_QUERY_TIMEOUT,
        )?)
    }

    /// The underlying bus (escape hatch for framework runtimes/drivers; not part
    /// of the normal authoring path).
    pub fn bus(&self) -> &Bus {
        &self.bus
    }
}

/// Builder returned by [`SetupContext::subscribe`]; pick `latest()` (keep-last-1)
/// or `subscriber()` (drop-oldest ring).
pub struct SubscribeBuilder<R: RuntimeFields, B> {
    bus: Bus,
    topic: Topic<PubSub<B>>,
    depth: usize,
    _runtime: PhantomData<fn() -> R>,
}

impl<R, B> SubscribeBuilder<R, B>
where
    R: RuntimeFields,
    B: ContractBody<Api = R::Api>,
{
    /// Override the ring depth for a `subscriber()` (ignored by `latest()`).
    pub fn depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    /// A keep-last-1 view of the topic.
    pub async fn latest(self) -> crate::Result<Latest<B>> {
        Ok(Latest::new(&self.bus, &self.topic).await?)
    }

    /// A drop-oldest ring subscription.
    pub async fn subscriber(self) -> crate::Result<Subscriber<B>> {
        Ok(Subscriber::new(&self.bus, &self.topic, self.depth).await?)
    }
}

/// Per-step logical-time context (D34). `time()` is in the same domain as every
/// bus `produced_at_ns`.
#[derive(Clone, Copy, Debug)]
pub struct StepContext {
    epoch: u64,
    step_index: u64,
    time_ns: u64,
    dt_ns: u64,
    missed_ticks: u32,
}

impl StepContext {
    pub(crate) fn new(
        epoch: u64,
        step_index: u64,
        time_ns: u64,
        dt_ns: u64,
        missed_ticks: u32,
    ) -> Self {
        StepContext {
            epoch,
            step_index,
            time_ns,
            dt_ns,
            missed_ticks,
        }
    }

    /// Logical robot time for this step.
    pub fn time(&self) -> LogicalTime {
        LogicalTime::new(self.epoch, self.time_ns)
    }

    /// The clock epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Monotonic step counter within the epoch.
    pub fn step_index(&self) -> u64 {
        self.step_index
    }

    /// Nanoseconds since the previous step.
    pub fn dt_ns(&self) -> u64 {
        self.dt_ns
    }

    /// `dt` as a [`Duration`].
    pub fn dt(&self) -> Duration {
        Duration::from_nanos(self.dt_ns)
    }

    /// Ticks collapsed into this step after an overrun (D34).
    pub fn missed_ticks(&self) -> u32 {
        self.missed_ticks
    }
}

/// Context for `#[shutdown]`: graceful park/stop/flush before bus close (D24/D43i).
#[derive(Clone, Copy, Debug)]
pub struct ShutdownContext {
    grace: Duration,
}

impl ShutdownContext {
    pub(crate) fn new(grace: Duration) -> Self {
        ShutdownContext { grace }
    }

    /// The bounded grace period the runner allows before forcing shutdown.
    pub fn grace(&self) -> Duration {
        self.grace
    }
}
