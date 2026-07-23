//! Participant contexts: `SetupContext` (IO construction), `StepContext` (logical
//! time per scheduled step), and `ShutdownContext`.

use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::bus::{LogicalTime, OwnerCap};
use crate::model::v0::Robot;
use crate::participant::launch::ExecutionDeviceId;
use crate::participant::managed::{ManagedTaskPolicy, ManagedTasks};
use phoxal_bus::Bus;

/// The sole IO-construction point, handed to `#[setup]` (D18). Builders live
/// on [`SetupContextApiExt`](super::api::SetupContextApiExt) (`R: Participant`);
/// see that trait for the side-brand/owner-capability contract each builder
/// enforces.
pub struct SetupContext<R> {
    bus: Bus,
    owner_cap: OwnerCap,
    robot: Option<Arc<Robot>>,
    robot_root: Option<PathBuf>,
    component_instance: Option<String>,
    execution_device_id: Option<ExecutionDeviceId>,
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
        owner_cap: OwnerCap,
        robot: Option<Arc<Robot>>,
        robot_root: Option<PathBuf>,
        component_instance: Option<String>,
        execution_device_id: Option<ExecutionDeviceId>,
    ) -> Self {
        SetupContext {
            bus,
            owner_cap,
            robot,
            robot_root,
            component_instance,
            execution_device_id,
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

    /// The runner-minted owner capability (plan #00 L2). In-crate accessor the
    /// NEW model's `SetupContextApiExt::owner_capability` (`participant::api`)
    /// reads the raw field through.
    pub(crate) fn owner_cap(&self) -> OwnerCap {
        self.owner_cap
    }

    /// The bound `robot.components` instance, if any. In-crate accessor for the
    /// driver/simulator `component()` builders (`participant::api`).
    pub(crate) fn component_instance(&self) -> Option<&str> {
        self.component_instance.as_deref()
    }

    /// The bounded project/deployment identity supplied by the supervisor.
    pub(crate) fn execution_device_id_ref(&self) -> Option<&ExecutionDeviceId> {
        self.execution_device_id.as_ref()
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
