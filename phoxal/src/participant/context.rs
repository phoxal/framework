//! Participant contexts: `SetupContext` (IO construction), `ResetContext`
//! (simulation execution replacement), `StepContext` (logical time per
//! scheduled step), and `ShutdownContext`.

use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::bus::{RobotInstant, StepToken, TimelineId};
use crate::model::v0::Robot;
use crate::participant::managed::{ManagedTaskPolicy, ManagedTasks};
use phoxal_bus::Bus;

/// The sole IO-construction point, handed to `#[setup]` (D18). Builders live
/// on [`SetupContextApiExt`](super::api::SetupContextApiExt) (`R: Participant`);
/// see that trait for the side-brand contract each builder
/// enforces.
pub struct SetupContext<R> {
    bus: Bus,
    robot: Option<Arc<Robot>>,
    robot_root: Option<PathBuf>,
    component_instance: Option<String>,
    managed_tasks: ManagedTasks,
    _runtime: PhantomData<fn() -> R>,
}

/// Unbounded on `R`: these just store/read raw fields or spawn/track tasks
/// generically, and [`SetupContextApiExt`](super::api::SetupContextApiExt)
/// (`participant::api`, a sibling module) calls them from its own `R:
/// Participant`-bound `impl`.
impl<R> SetupContext<R> {
    pub(crate) fn new(
        bus: Bus,
        robot: Option<Arc<Robot>>,
        robot_root: Option<PathBuf>,
        component_instance: Option<String>,
    ) -> Self {
        SetupContext {
            bus,
            robot,
            robot_root,
            component_instance,
            managed_tasks: ManagedTasks::default(),
            _runtime: PhantomData,
        }
    }

    /// Spawn a runner-owned, long-lived background task (sensor polling loop,
    /// serial/USB reader, async IO pump) under the default
    /// [`ManagedTaskPolicy::FaultOnExit`] policy.
    ///
    /// This is the framework-tracked alternative to a raw `tokio::spawn`:
    /// **checked participants must not `tokio::spawn` long-lived work**, because
    /// the runner cannot observe, cancel, or join a detached task. A managed
    /// task, by contrast, is watched for the rest of the participant's
    /// lifetime - if it panics or returns while `FaultOnExit` applies, the
    /// runner treats that as a runtime fault (participant marked `Failed`,
    /// lose the participant Liveliness token) exactly as it would a `#[step]` bug it
    /// cannot recover from. At shutdown the runner cancels every managed task
    /// as the shutdown sequence starts and joins it within the same grace
    /// budget as `#[shutdown]` (see [`ShutdownContext::grace`]), before the bus
    /// closes.
    ///
    /// `name` is a short diagnostic label (e.g. `"serial-reader"`) surfaced in
    /// runner logs on fault or on an unjoined-at-shutdown report; it does not
    /// need to be unique. Use [`Self::spawn_managed_with`] for setup-time work
    /// that is expected to finish on its own ([`ManagedTaskPolicy::AllowExit`]).
    pub fn spawn_managed<F>(&mut self, name: impl Into<String>, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.spawn_managed_with(name, ManagedTaskPolicy::FaultOnExit, future);
    }

    /// [`Self::spawn_managed`] with an explicit [`ManagedTaskPolicy`].
    ///
    /// Use [`ManagedTaskPolicy::AllowExit`] for setup-time-only work (a
    /// background warm-up, a best-effort cache prime) whose completion should
    /// never fault the participant; anything meant to run for the participant's
    /// whole lifetime should keep the [`ManagedTaskPolicy::FaultOnExit`]
    /// default from [`Self::spawn_managed`].
    pub fn spawn_managed_with<F>(
        &mut self,
        name: impl Into<String>,
        policy: ManagedTaskPolicy,
        future: F,
    ) where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.managed_tasks.spawn(name, policy, future);
    }

    /// Hand the managed-task registry accumulated during `#[setup]` to the
    /// runner, which then owns watching/cancelling/joining them for the rest of
    /// the participant's lifetime. Called exactly once, after `#[setup]`
    /// returns.
    pub(crate) fn take_managed_tasks(&mut self) -> ManagedTasks {
        std::mem::take(&mut self.managed_tasks)
    }

    /// The underlying bus. Not on the default checked-participant surface (plan #00
    /// DoD #11 / plan #07): normal participants and examples cannot reach around
    /// the typed handle builders. Privileged participants that genuinely need raw
    /// access go through `phoxal::raw` (`Bus::open` + `run_with_bus`) or the
    /// tool-only [`Self::raw_bus`] accessor.
    pub(crate) fn bus(&self) -> &Bus {
        &self.bus
    }

    /// The bound `robot.components` instance, if any. In-crate accessor for the
    /// driver/simulator `component()` builders (`participant::api`).
    pub(crate) fn component_instance(&self) -> Option<&str> {
        self.component_instance.as_deref()
    }

    /// The resolved robot model, if bound. In-crate accessor for
    /// [`SetupContextApiExt::robot`](super::api::SetupContextApiExt::robot).
    pub(crate) fn robot_ref(&self) -> Option<&Robot> {
        self.robot.as_deref()
    }

    /// The robot root directory, if bound. In-crate accessor for
    /// [`SetupContextApiExt::robot_root`](super::api::SetupContextApiExt::robot_root).
    pub(crate) fn robot_root_ref(&self) -> Option<&Path> {
        self.robot_root.as_deref()
    }
}

/// Per-step context: the robot instant this step reached, plus the capability
/// to publish state at it.
///
/// The [`StepToken`] is what a [`StatePublisher`](crate::bus::StatePublisher)
/// requires, and the runner is the only minter on the documented surface - so
/// a participant publishes state at the instant it actually reached, or not at
/// all (#952 section D; `phoxal::raw`'s docs state exactly how strong that is).
#[derive(Clone, Copy, Debug)]
pub struct StepContext {
    token: StepToken,
    step_index: u64,
    dt: Duration,
    missed_ticks: u32,
}

/// Context for `#[reset]`: the runner observed a different timeline and is
/// about to begin releasing steps for that world history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetContext {
    previous_timeline: TimelineId,
    new_timeline: TimelineId,
}

impl ResetContext {
    pub(crate) fn new(previous_timeline: TimelineId, new_timeline: TimelineId) -> Self {
        Self {
            previous_timeline,
            new_timeline,
        }
    }

    /// The world history whose derived state must be discarded.
    pub fn previous_timeline(&self) -> TimelineId {
        self.previous_timeline
    }

    /// The newly active world history.
    pub fn new_timeline(&self) -> TimelineId {
        self.new_timeline
    }
}

impl StepContext {
    pub(crate) fn new(token: StepToken, step_index: u64, dt: Duration, missed_ticks: u32) -> Self {
        StepContext {
            token,
            step_index,
            dt,
            missed_ticks,
        }
    }

    /// The capability to publish state at this step's instant.
    pub fn token(&self) -> &StepToken {
        &self.token
    }

    /// The robot instant this step reached.
    pub fn now(&self) -> RobotInstant {
        crate::bus::StepStamp::instant(&self.token)
    }

    /// The world history this step belongs to.
    pub fn timeline(&self) -> TimelineId {
        self.now().timeline()
    }

    /// Monotonic step counter within the timeline.
    pub fn step_index(&self) -> u64 {
        self.step_index
    }

    /// Robot time since the previous step.
    pub fn dt(&self) -> Duration {
        self.dt
    }

    /// Ticks collapsed into this step after an overrun (D34).
    pub fn missed_ticks(&self) -> u32 {
        self.missed_ticks
    }
}

/// Context for `#[shutdown]`: graceful park/stop/flush before bus close (D24/D43i).
///
/// The runner bounds the whole `#[shutdown]` hook by [`grace`](Self::grace): if the
/// hook is still running at the deadline, the runner logs, drops the hook, and
/// proceeds to bus close anyway so the process never leaks. Treat [`grace`](Self::grace)
/// as a budget for any internal flush/park deadlines and return before it elapses.
#[derive(Clone, Copy, Debug)]
pub struct ShutdownContext {
    grace: Duration,
}

impl ShutdownContext {
    pub(crate) fn new(grace: Duration) -> Self {
        ShutdownContext { grace }
    }

    /// The bounded grace period the runner allows the hook before it forces bus
    /// close. Sourced from `ParticipantLaunch::shutdown_grace_ms`.
    pub fn grace(&self) -> Duration {
        self.grace
    }
}
