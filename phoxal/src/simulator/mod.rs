//! The external simulator host SDK.
//!
//! A simulator is not a robot participant. It owns a world, stands in for
//! several component-driver identities at once, and follows its own process
//! lifecycle - Webots decides when a step happens, not a Phoxal scheduler. So
//! there is no role attribute, no runner and no `SetupContext` here: there is
//! [`SimulatorSession`], and it is the whole of what the framework hands a
//! world adapter.
//!
//! ```text
//! SimulatorSession::connect       one execution, one external bus session
//!     .present(participant)       stand in for one component driver
//!     .sample_publisher(topic)    typed component IO, owner side
//!     .take_world_time()          -> WorldTime, moved to the step thread
//!     .close()                    drop presence, then close the transport
//! ```
//!
//! # World time is a separate value on purpose
//!
//! Everything above is asynchronous and lives with the adapter's Tokio
//! runtime; the step loop is a synchronous thread that owns the world outright,
//! because every simulator call blocks and must come from the thread that
//! opened the devices. [`WorldTime`] is the part that belongs on that thread -
//! the timeline authority and the world-clock publisher - so it is taken once,
//! moved there, and cannot be taken twice.
//!
//! The order a step commits in is the contract, and [`WorldTime`] is shaped to
//! make it the easy path: the world advances,
//! [`completed_step`](WorldTime::completed_step) mints the one token for that
//! advance, every capability publishes with that token, and
//! [`publish_clock`](WorldTime::publish_clock) closes the step. A reader that
//! has seen a step's clock has already seen that step's outputs.
//!
//! # What it does not hand out
//!
//! No bus owner, no session construction, no timeline authority, no
//! unrestricted delegated presence. Those are how this module does its job,
//! not what it offers: an adapter that held them would be holding framework
//! transport ownership, and the typed handles could then promise nothing.

use std::collections::BTreeMap;

use crate::bus::handle::publisher::WorldClockPublisher;
use crate::bus::handle::stamp::TimelineAuthority;
use crate::bus::session::{BusConfig, BusOwner};
use crate::bus::{
    BusError, BusHandle, Endpoint, ParticipantReadyEvents, ParticipantReadyToken, Publish,
    RobotEndpoint, Sample, SamplePublisher, Setpoint, SetpointReceiver, SourceLabel,
    SourceLabelError, State, StatePublisher, Subscribe, Topic, WorldStepToken,
};
use crate::identity::{ExecutionId, ParticipantId, TimelineId};
use crate::runtime::api::simulation::Clock;

/// A failure while attaching a simulator to one execution, or while operating
/// the session it opened.
#[derive(Debug, thiserror::Error)]
pub enum SimulatorError {
    /// No router answered at the configured endpoint.
    #[error(
        "no Phoxal execution is reachable at {connect}; start the supervisor before the simulation"
    )]
    NoExecution { connect: String },

    /// More than one execution answered, so the endpoint did not identify one
    /// world to simulate.
    #[error(
        "{count} Phoxal executions are reachable at {connect}, which must identify exactly one: {executions:?}"
    )]
    MultipleExecutions {
        connect: String,
        count: usize,
        executions: Vec<ExecutionId>,
    },

    /// The diagnostic label could not be represented by the framework bus.
    #[error(transparent)]
    SourceLabel(#[from] SourceLabelError),

    /// The underlying transport failed.
    #[error(transparent)]
    Bus(#[from] BusError),

    /// [`SimulatorSession::take_world_time`] was called a second time.
    #[error("this session's world time has already been taken")]
    WorldTimeTaken,
}

/// Inputs for one simulator session against one execution.
#[derive(Clone, Debug)]
pub struct SimulatorConnectOptions {
    /// The router endpoint to join. It must identify exactly one execution.
    pub connect: String,
    /// A bounded diagnostic label this simulator's own traffic carries. It
    /// never affects routing, authority, or Ready admission - it only says
    /// which external client produced a sample.
    pub label: String,
}

impl SimulatorConnectOptions {
    #[must_use]
    pub fn new(connect: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            connect: connect.into(),
            label: label.into(),
        }
    }
}

/// One simulator process attached to one execution.
///
/// It owns the external bus session, the presence it stands in with, and -
/// until it is taken - the world's time.
pub struct SimulatorSession {
    owner: Option<BusOwner>,
    bus: BusHandle,
    execution: ExecutionId,
    /// One delegated Ready lease per component driver this process stands in
    /// for, keyed so a repeated `present` is idempotent rather than a second
    /// lease under the same identity.
    presence: BTreeMap<ParticipantId, ParticipantReadyToken>,
    world_time: Option<WorldTime>,
}

impl std::fmt::Debug for SimulatorSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SimulatorSession")
            .field("execution", &self.execution)
            .field("presented", &self.presence.len())
            .field("world_time_taken", &self.world_time.is_none())
            .finish_non_exhaustive()
    }
}

impl SimulatorSession {
    /// The executions reachable at `connect`.
    ///
    /// The execution id is never an argument to a simulator: a router's session
    /// id *is* the execution, so asking the transport is the only answer that
    /// cannot disagree with the run actually in progress.
    ///
    /// # Errors
    ///
    /// Returns [`SimulatorError::Bus`] when the endpoint cannot be probed.
    pub async fn probe(connect: &str) -> Result<Vec<ExecutionId>, SimulatorError> {
        Ok(BusOwner::probe_routers(connect).await?)
    }

    /// Join the sole execution reachable at `options.connect`.
    ///
    /// # Errors
    ///
    /// Returns [`SimulatorError::NoExecution`] or
    /// [`SimulatorError::MultipleExecutions`] when the endpoint does not name
    /// exactly one execution, and [`SimulatorError::Bus`] when the session
    /// cannot be opened.
    pub async fn connect(options: SimulatorConnectOptions) -> Result<Self, SimulatorError> {
        let executions = Self::probe(&options.connect).await?;
        let execution = match executions.as_slice() {
            [only] => *only,
            [] => {
                return Err(SimulatorError::NoExecution {
                    connect: options.connect,
                });
            }
            many => {
                let mut executions = many.to_vec();
                executions.sort_by_key(ToString::to_string);
                return Err(SimulatorError::MultipleExecutions {
                    connect: options.connect,
                    count: executions.len(),
                    executions,
                });
            }
        };
        let label = SourceLabel::new(options.label)?;
        Self::open(
            BusConfig::for_external(execution, Some(label), vec![options.connect]),
            execution,
        )
        .await
    }

    /// Open an in-process session for a world with no router, which is how the
    /// ordering and presence proofs below run against the real transport.
    #[cfg(test)]
    pub(crate) async fn in_process(label: &str) -> Result<Self, SimulatorError> {
        let execution = ExecutionId::mint();
        Self::open(
            BusConfig::for_external(execution, Some(SourceLabel::new(label)?), Vec::new()),
            execution,
        )
        .await
    }

    async fn open(config: BusConfig, execution: ExecutionId) -> Result<Self, SimulatorError> {
        let (owner, bus) = BusOwner::open(config).await?;
        let world_time = match WorldTime::open(&bus) {
            Ok(world_time) => world_time,
            Err(error) => {
                let _ = owner.close().await;
                return Err(error);
            }
        };
        Ok(Self {
            owner: Some(owner),
            bus,
            execution,
            presence: BTreeMap::new(),
            world_time: Some(world_time),
        })
    }

    /// The execution this simulator joined.
    #[must_use]
    pub fn execution(&self) -> ExecutionId {
        self.execution
    }

    /// Publish a component capability's measurements on the owner side.
    ///
    /// The topic comes from the robot api tree's owner side, because a
    /// simulator *is* the owner of every capability it stands in for:
    /// `api::topics().component(&instance)?.encoder(&capability)?.sample().owner()`.
    ///
    /// # Errors
    ///
    /// Returns [`SimulatorError::Bus`] when the publisher cannot be attached.
    pub fn sample_publisher<E>(
        &self,
        topic: Topic<Publish<E>>,
    ) -> Result<SamplePublisher<E>, SimulatorError>
    where
        E: RobotEndpoint + Endpoint<Semantics = Sample>,
    {
        Ok(SamplePublisher::new(self.bus.clone(), &topic)?)
    }

    /// Publish a component capability's current state on the owner side.
    ///
    /// # Errors
    ///
    /// Returns [`SimulatorError::Bus`] when the publisher cannot be attached.
    pub fn state_publisher<E>(
        &self,
        topic: Topic<Publish<E>>,
    ) -> Result<StatePublisher<E>, SimulatorError>
    where
        E: RobotEndpoint + Endpoint<Semantics = State>,
    {
        Ok(StatePublisher::new(self.bus.clone(), &topic)?)
    }

    /// Receive the setpoints the graph sends a component capability this
    /// simulator owns.
    ///
    /// Admission is deliberately not a parameter. The receiver keeps one
    /// pending value per producer and the adapter offers that whole set to a
    /// [`FixedSourceLease`](crate::bus::FixedSourceLease) it owns, so a rogue
    /// producer cannot coalesce the authorised one away before the lease has
    /// judged it. Folding the lease in here would decide for the adapter when
    /// that judgement happens.
    ///
    /// # Errors
    ///
    /// Returns [`SimulatorError::Bus`] when the subscription cannot be
    /// declared.
    pub async fn setpoint_receiver<E>(
        &self,
        topic: Topic<Subscribe<E>>,
    ) -> Result<SetpointReceiver<E>, SimulatorError>
    where
        E: RobotEndpoint + Endpoint<Semantics = Setpoint>,
    {
        Ok(SetpointReceiver::new(&self.bus, &topic).await?)
    }

    /// Observe one participant's Ready leases, which is the evidence a
    /// fixed-source admission decision stands on.
    ///
    /// # Errors
    ///
    /// Returns [`SimulatorError::Bus`] when the observer cannot be declared.
    pub async fn participant_ready_events(
        &self,
        participant: &ParticipantId,
    ) -> Result<ParticipantReadyEvents, SimulatorError> {
        Ok(self.bus.participant_ready_events_for(participant).await?)
    }

    /// Stand in for one component driver's presence until this session closes.
    ///
    /// A simulated robot must read as exactly as present as the same robot on
    /// hardware, so the adapter declares one lease per component instance that
    /// declares a `driver` block - the same set a launcher would start
    /// processes for. Declaring the same identity twice is a no-op rather than
    /// a second lease.
    ///
    /// Presence is a promise that the contracts are already served, so call it
    /// after the capability handles are bound.
    ///
    /// # Errors
    ///
    /// Returns [`SimulatorError::Bus`] when the lease cannot be declared.
    pub async fn present(&mut self, participant: &ParticipantId) -> Result<(), SimulatorError> {
        if self.presence.contains_key(participant) {
            return Ok(());
        }
        let Some(owner) = self.owner.as_ref() else {
            return Ok(());
        };
        let token = owner.declare_participant_ready_as(participant).await?;
        self.presence.insert(participant.clone(), token);
        Ok(())
    }

    /// Take this session's world time, once.
    ///
    /// The returned value is `Send` and is meant to move onto the simulator's
    /// own step thread. A world has one hand, so a second call fails rather
    /// than handing out a second one.
    ///
    /// # Errors
    ///
    /// Returns [`SimulatorError::WorldTimeTaken`] when it has already been
    /// taken.
    pub fn take_world_time(&mut self) -> Result<WorldTime, SimulatorError> {
        self.world_time.take().ok_or(SimulatorError::WorldTimeTaken)
    }

    /// Close deterministically: drop presence first, then the transport.
    ///
    /// The order is the point. Dropping presence while the wheels were still
    /// turning would let a reader believe the drivers are already gone, so the
    /// adapter parks its world, then calls this.
    pub async fn close(mut self) {
        self.presence.clear();
        self.world_time = None;
        if let Some(owner) = self.owner.take() {
            owner.close().await;
        }
    }
}

/// The world's own time: the timeline this process owns, and the clock hand it
/// closes each step with.
///
/// Taken once from a [`SimulatorSession`] and moved to the thread that advances
/// the world. There is no way to make a second one, in this process or any
/// other reachable API: a world has one hand.
pub struct WorldTime {
    authority: TimelineAuthority,
    clock: WorldClockPublisher<Clock>,
}

impl std::fmt::Debug for WorldTime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorldTime")
            .field("timeline", &self.authority.timeline())
            .finish_non_exhaustive()
    }
}

impl WorldTime {
    fn open(bus: &BusHandle) -> Result<Self, SimulatorError> {
        let authority = TimelineAuthority::mint(TimelineId::mint())?;
        let clock = WorldClockPublisher::mint(
            bus.clone(),
            &crate::runtime::api::topics().simulation().clock().owner(),
        )?;
        Ok(Self { authority, clock })
    }

    /// Mint the one token for a completed world advance at `time_ns`.
    ///
    /// Every output of that advance is stamped with this token, and
    /// [`publish_clock`](Self::publish_clock) closes the step with it.
    pub fn completed_step(&mut self, time_ns: u64) -> WorldStepToken {
        self.authority.completed_step(time_ns)
    }

    /// Begin a new world history, which is what a rewind or a reset is.
    ///
    /// Robot instants on the previous timeline are not comparable with the new
    /// ones, and every receiver treats the change as the discontinuity it is.
    pub fn replace_timeline(&mut self) {
        self.authority.replace_timeline(TimelineId::mint());
    }

    /// The timeline this world is currently on.
    #[must_use]
    pub fn timeline(&self) -> TimelineId {
        self.authority.timeline()
    }

    /// Close a step by publishing the authoritative world clock for it.
    ///
    /// Publish every output of the step first: a reader that has seen the clock
    /// has, by then, already seen everything that step produced.
    ///
    /// # Errors
    ///
    /// Returns [`SimulatorError::Bus`] when the clock cannot be admitted to the
    /// outbound lane.
    pub fn publish_clock(
        &mut self,
        step: &WorldStepToken,
        clock: Clock,
    ) -> Result<(), SimulatorError> {
        Ok(self.clock.publish(step, clock)?)
    }
}

#[cfg(test)]
mod world_session_tests;
