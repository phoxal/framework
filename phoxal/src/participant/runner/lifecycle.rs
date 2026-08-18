//! Participant startup, Ready transition, run completion, and teardown handoff.
//!
//! The serialized scheduler loop itself lives in [`super::event_loop`]. This
//! module owns the resources that surround that loop and keeps every terminal
//! transition typed until the final framework result is assembled.

use std::future::Future;
use std::time::Duration;

use crate::bundle::RuntimeBundle;
use crate::bus::{BusFault, BusHandle, BusOwner, ParticipantReadyToken};
use crate::identity::ParticipantId;
use crate::participant::api::Participant;
use crate::participant::bus_log::{self, BusLogTask};
use crate::participant::clock::simulation::SimulationClock;
use crate::participant::clock::{ClockMode, ClockReading, ClockSource, TimeUnsynchronized};
use crate::participant::context::{SetupContext, TimelineRetention};
use crate::participant::managed::{ManagedTaskExit, ManagedTaskPolicy, ManagedTasks};
use crate::participant::runtime_performance::{RuntimePerformance, RuntimePerformancePublisher};
use crate::participant::scheduler::simulation::SimulationClockHandle;
use crate::participant::scheduler::{AnyStepScheduler, StepSchedule};

use super::ShutdownController;
use super::query::QuerySurface;
use super::startup::PreparedRun;
use super::teardown::{
    ShutdownDeadline, Teardown, TeardownReport, abandon_setup, abandon_startup, combine,
};

/// The runner's bus lifetime mode.
///
/// A production run carries the unique [`BusOwner`] all the way through the
/// lifecycle. The borrowed variant is only constructed by the explicit test
/// harness, whose caller owns the bus session and therefore cannot declare a
/// Ready lease or close the transport.
pub(crate) enum BusLease {
    Owned(BusOwner),
    #[cfg(feature = "test-harness")]
    Borrowed,
}

/// Why the main loop stopped.
///
/// Every variant but [`LoopExit::ShutdownRequested`] means the same thing to
/// the caller: run ordinary teardown, which parks hardware through
/// `Participant::shutdown`, then report a failure so supervisor policy decides
/// what happens next.
#[derive(Debug)]
pub(crate) enum LoopExit {
    /// The host asked the participant to stop. The only exit that is not a
    /// failure.
    ShutdownRequested,
    /// A runner-owned task violated its completion policy.
    ManagedTaskFaulted(ManagedTaskExit),
    /// An owner-owned bus worker exited unexpectedly, so continuing with
    /// frozen transport input would make Ready untrustworthy.
    BusFaulted(BusFault),
    /// The clock stopped being trustworthy, so no further step could be timed.
    ClockDisciplineLost(TimeUnsynchronized),
    /// `Participant::reset` refused the replacement world history.
    ResetFailed(anyhow::Error),
    /// A scheduled state transition failed. Step failures are terminal so the
    /// runner can park the participant immediately instead of continuing with
    /// state whose invariants the transition may have left unknown.
    StepFailed(anyhow::Error),
    /// The bounded runner-owned query reply queue could not accept a response
    /// without making the serialized owner await transport back-pressure.
    QueryDispatchFailed(anyhow::Error),
}

impl LoopExit {
    pub(crate) fn into_result(self) -> crate::Result<()> {
        match self {
            Self::ShutdownRequested => Ok(()),
            Self::ManagedTaskFaulted(exit) => Err(ParticipantFault::ManagedTask(exit).into()),
            Self::BusFaulted(fault) => Err(ParticipantFault::Bus(fault).into()),
            Self::ClockDisciplineLost(reason) => {
                Err(ParticipantFault::Clock(ClockDisciplineLost { reason }).into())
            }
            Self::ResetFailed(error) => Err(ParticipantFault::Reset(error).into()),
            Self::StepFailed(error) => Err(ParticipantFault::Step(error).into()),
            Self::QueryDispatchFailed(error) => Err(ParticipantFault::Query(error).into()),
        }
    }
}

/// The failure a participant reports when it cannot trust its own clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("clock discipline lost: {reason}")]
pub(crate) struct ClockDisciplineLost {
    pub(crate) reason: TimeUnsynchronized,
}

/// The typed primary fault for a participant run.
#[derive(Debug)]
pub(crate) enum ParticipantFault {
    ManagedTask(ManagedTaskExit),
    Bus(BusFault),
    Clock(ClockDisciplineLost),
    Reset(anyhow::Error),
    Step(anyhow::Error),
    Query(anyhow::Error),
}

impl std::fmt::Display for ParticipantFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ManagedTask(error) => error.fmt(formatter),
            Self::Bus(error) => write!(formatter, "bus transport failed: {error}"),
            Self::Clock(error) => error.fmt(formatter),
            Self::Reset(error) => write!(formatter, "reset failed: {error}"),
            Self::Step(error) => write!(formatter, "step failed: {error}"),
            Self::Query(error) => write!(formatter, "query dispatch failed: {error}"),
        }
    }
}

impl std::error::Error for ParticipantFault {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ManagedTask(error) => Some(error),
            Self::Bus(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::Reset(error) | Self::Step(error) | Self::Query(error) => Some(error.as_ref()),
        }
    }
}

/// The runner's effective timestamp clock, chosen once the scheduler is built.
pub(crate) enum RunnerClock<C: ClockSource> {
    Delegated(C),
    Simulation(SimulationClock),
}

impl<C: ClockSource> ClockSource for RunnerClock<C> {
    fn read(&self) -> ClockReading {
        match self {
            Self::Delegated(clock) => clock.read(),
            Self::Simulation(clock) => clock.read(),
        }
    }
}

/// Select the runner's timestamp clock from the scheduler shape and the
/// launch-validated clock.
///
/// A disabled scheduler does not mean a disabled clock: a stepless real
/// participant still dates the state it serves, so it keeps the host clock the
/// launch built. Only a simulated participant reads its instants from somewhere
/// else, and the scheduler it runs on is that source.
pub(crate) fn runner_clock<C: ClockSource>(
    scheduler: &AnyStepScheduler,
    clock: Option<C>,
) -> Result<RunnerClock<C>, ClockDisciplineLost> {
    match scheduler {
        AnyStepScheduler::Simulation(simulation) => {
            Ok(RunnerClock::Simulation(simulation.simulation_clock()))
        }
        // Both remaining shapes are real launches - one with a cadence, one
        // without - and a real launch always carries a host clock. Reaching the
        // `None` arm means the caller assembled a real run without one, which
        // reads as a clock this process cannot read; the participant fails
        // rather than dating its state on an invented instant.
        AnyStepScheduler::Real(_) | AnyStepScheduler::Disabled => match clock {
            Some(clock) => Ok(RunnerClock::Delegated(clock)),
            None => Err(ClockDisciplineLost {
                reason: TimeUnsynchronized::ClockFault,
            }),
        },
    }
}

/// Framework tasks that must be registered before setup can declare Ready.
pub(crate) struct RunnerTasks {
    pub(crate) simulation_clock: Option<SimulationClockHandle>,
    pub(crate) bus_log: BusLogTask,
    pub(crate) query_reply_delay: Option<Duration>,
}

/// Inputs for the single startup transition. Grouping the ownership boundary
/// here keeps the scheduler, clock, selected runtime record, and task set
/// explicit without threading a long parameter list through `Runner::start`.
pub(crate) struct StartInputs<R: Participant, C: ClockSource> {
    pub(crate) bus: BusHandle,
    pub(crate) session: BusLease,
    pub(crate) participant_id: ParticipantId,
    pub(crate) shutdown_grace: Duration,
    pub(crate) bundle: Option<RuntimeBundle>,
    pub(crate) config: R::Config,
    pub(crate) clock: RunnerClock<C>,
    pub(crate) scheduler: AnyStepScheduler,
    pub(crate) schedule: Option<StepSchedule>,
    pub(crate) clock_mode: ClockMode,
    pub(crate) tasks: RunnerTasks,
}

pub(crate) enum StartOutcome<T> {
    Ready(T),
    Terminal {
        result: crate::Result<()>,
        deadline: ShutdownDeadline,
        session: BusLease,
    },
}

fn startup_terminal<T>(
    primary: crate::Result<()>,
    report: TeardownReport,
    deadline: ShutdownDeadline,
    session: BusLease,
) -> StartOutcome<T> {
    StartOutcome::Terminal {
        result: combine(primary, report),
        deadline,
        session,
    }
}

async fn startup_teardown<T, R>(
    managed_tasks: ManagedTasks,
    participant: &R,
    api: &R::Api,
    state: &mut R::State,
    shutdown_grace: Duration,
    primary: crate::Result<()>,
    session: BusLease,
) -> StartOutcome<T>
where
    R: Participant,
{
    let deadline = ShutdownDeadline::from_now(shutdown_grace);
    let report = Teardown {
        managed_tasks,
        deadline,
    }
    .run(participant, api, state)
    .await;
    startup_terminal(primary, report, deadline, session)
}

/// Build the live scheduler after the potentially slow bus connection, then
/// run setup, Ready acquisition, the serialized event loop, and teardown.
pub(crate) async fn run<R, C, S>(
    prepared: PreparedRun<R, C>,
    shutdown: &mut ShutdownController<S>,
) -> crate::Result<()>
where
    R: Participant,
    C: ClockSource,
    S: Future<Output = ()>,
{
    let PreparedRun {
        bus,
        session,
        participant_id,
        shutdown_grace,
        bundle,
        config,
        clock_mode,
        clock,
        query_reply_delay,
    } = prepared;
    let (bus_logs, bus_log_task) = bus_log::attach(bus.clone());
    let schedule = R::__step_schedule();
    let now = if clock_mode == ClockMode::Real {
        let reading = clock
            .as_ref()
            .map(ClockSource::read)
            .unwrap_or(ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault));
        match reading {
            ClockReading::Synchronized(_) => reading.instant(),
            ClockReading::Unsynchronized(reason) => {
                let result = close_session_with_result(
                    Err(ClockDisciplineLost { reason }.into()),
                    session,
                    ShutdownDeadline::from_now(shutdown_grace),
                )
                .await;
                bus_logs.shutdown();
                return result;
            }
        }
    } else {
        None
    };
    let (scheduler, clock_handle) =
        match AnyStepScheduler::for_clock_mode(clock_mode, schedule, now) {
            Ok(value) => value,
            Err(error) => {
                let result = close_session_with_result(
                    Err(error),
                    session,
                    ShutdownDeadline::from_now(shutdown_grace),
                )
                .await;
                bus_logs.shutdown();
                return result;
            }
        };
    let effective_clock = match runner_clock(&scheduler, clock) {
        Ok(clock) => clock,
        Err(error) => {
            let result = close_session_with_result(
                Err(error.into()),
                session,
                ShutdownDeadline::from_now(shutdown_grace),
            )
            .await;
            bus_logs.shutdown();
            return result;
        }
    };
    let start = Runner::<R, C>::start(
        StartInputs {
            bus,
            session,
            participant_id,
            shutdown_grace,
            bundle,
            config,
            clock: effective_clock,
            scheduler,
            schedule,
            clock_mode,
            tasks: RunnerTasks {
                simulation_clock: clock_handle,
                bus_log: bus_log_task,
                query_reply_delay,
            },
        },
        shutdown,
    )
    .await;
    let result = match start {
        StartOutcome::Ready(runner) => runner.run(shutdown).await,
        StartOutcome::Terminal {
            result,
            deadline,
            session,
        } => close_session_with_result(result, session, deadline).await,
    };
    bus_logs.shutdown();
    result
}

/// A participant, running.
pub(crate) struct Runner<R: Participant, C: ClockSource> {
    pub(crate) participant: R,
    pub(crate) api: R::Api,
    pub(crate) state: R::State,
    pub(crate) bus: BusHandle,
    pub(crate) session: BusLease,
    pub(crate) clock: RunnerClock<C>,
    pub(crate) scheduler: AnyStepScheduler,
    pub(crate) schedule: Option<StepSchedule>,
    pub(crate) clock_mode: ClockMode,
    pub(crate) timeline_retentions: Vec<TimelineRetention>,
    pub(crate) queries: Option<QuerySurface<R>>,
    pub(crate) runtime_performance_publisher: RuntimePerformancePublisher,
    pub(crate) runtime_performance: RuntimePerformance,
    pub(crate) managed_tasks: ManagedTasks,
    /// The participant's Ready lease. It is revoked before any shutdown work
    /// starts, so observers never see Ready while resources unwind.
    pub(crate) ready: Option<ParticipantReadyToken>,
    pub(crate) shutdown_grace: Duration,
}

impl<R: Participant, C: ClockSource> Runner<R, C> {
    /// Resolve the selected runtime record, run `Participant::setup`, and
    /// declare everything the participant announced before returning Ready.
    ///
    /// Every failure after `setup` succeeds still runs full teardown, so a
    /// server or Ready declaration failure cannot bypass the participant's
    /// hardware-safety hook.
    pub(crate) async fn start<S>(
        inputs: StartInputs<R, C>,
        shutdown: &mut ShutdownController<S>,
    ) -> StartOutcome<Self>
    where
        S: Future<Output = ()>,
    {
        let StartInputs {
            bus,
            session,
            participant_id,
            shutdown_grace,
            bundle,
            config,
            clock,
            scheduler,
            schedule,
            clock_mode,
            tasks,
        } = inputs;
        // The bundle (or explicit test harness) was already opened and this
        // participant's config deserialized before entering this
        // transport-owned startup path.
        let mut ctx = SetupContext::<R>::new(bus.clone(), bundle, participant_id.clone());
        ctx.spawn_managed_with(
            "bus-log-drain",
            ManagedTaskPolicy::Finite,
            tasks.bus_log.run(),
        );
        if let Some(handle) = tasks.simulation_clock {
            ctx.spawn_managed(
                "simulation-clock-ingest",
                simulation_clock_feed(bus.clone(), handle),
            );
        }
        let participant = R::__new();
        let setup = participant.setup(&mut ctx, config);
        let (mut state, api) = match tokio::select! {
            biased;
            _ = shutdown.wait() => {
                let deadline = ShutdownDeadline::from_now(shutdown_grace);
                let report = abandon_startup(ctx.take_managed_tasks(), deadline).await;
                return startup_terminal(Ok(()), report, deadline, session);
            }
            fault = bus.wait_for_fatal() => {
                let deadline = ShutdownDeadline::from_now(shutdown_grace);
                let error = abandon_setup(
                    ctx.take_managed_tasks(),
                    ParticipantFault::Bus(fault).into(),
                    deadline,
                ).await;
                return StartOutcome::Terminal { result: Err(error), deadline, session };
            }
            result = setup => result,
        } {
            Ok(pair) => pair,
            Err(error) => {
                let deadline = ShutdownDeadline::from_now(shutdown_grace);
                let error = abandon_setup(ctx.take_managed_tasks(), error, deadline).await;
                return StartOutcome::Terminal {
                    result: Err(error),
                    deadline,
                    session,
                };
            }
        };
        // From here on the runner - not `SetupContext` - owns watching the
        // tasks `ctx.spawn_managed(...)` started for an unexpected exit, and
        // cancelling/joining them at shutdown.
        let mut managed_tasks = ctx.take_managed_tasks();
        let timeline_retentions = ctx.take_timeline_retentions();
        let query_registrations = ctx.take_query_registrations();
        let query_reply_delay = tasks.query_reply_delay;

        let mut queries = match tokio::select! {
            biased;
            _ = shutdown.wait() => {
                return startup_teardown(
                    managed_tasks,
                    &participant,
                    &api,
                    &mut state,
                    shutdown_grace,
                    Ok(()),
                    session,
                ).await;
            }
            fault = bus.wait_for_fatal() => {
                return startup_teardown(
                    managed_tasks,
                    &participant,
                    &api,
                    &mut state,
                    shutdown_grace,
                    Err(ParticipantFault::Bus(fault).into()),
                    session,
                ).await;
            }
            result = QuerySurface::declare(
                &bus,
                query_registrations,
                &mut managed_tasks,
                query_reply_delay,
            ) => result,
        } {
            Ok(queries) => queries,
            Err(error) => {
                return startup_teardown(
                    managed_tasks,
                    &participant,
                    &api,
                    &mut state,
                    shutdown_grace,
                    Err(error),
                    session,
                )
                .await;
            }
        };

        // Setup and query declaration may have started tasks whose failure is
        // already ready to observe. Drain those completions before acquiring
        // the Ready lease so a failed critical task can never pass through a
        // transient Ready state.
        if let Some(exit) = managed_tasks.try_next_unexpected_exit() {
            if let Some(queries) = queries.take() {
                queries.close();
            }
            return startup_teardown(
                managed_tasks,
                &participant,
                &api,
                &mut state,
                shutdown_grace,
                Err(exit.into()),
                session,
            )
            .await;
        }
        if shutdown.is_requested() {
            if let Some(queries) = queries.take() {
                queries.close();
            }
            return startup_teardown(
                managed_tasks,
                &participant,
                &api,
                &mut state,
                shutdown_grace,
                Ok(()),
                session,
            )
            .await;
        }
        if let crate::bus::BusTerminal::Fatal(fault) = bus.terminal() {
            if let Some(queries) = queries.take() {
                queries.close();
            }
            return startup_teardown(
                managed_tasks,
                &participant,
                &api,
                &mut state,
                shutdown_grace,
                Err(ParticipantFault::Bus(fault).into()),
                session,
            )
            .await;
        }
        // Ready acquisition is itself a lifecycle boundary. Race the bus
        // declaration against already-supervised task completion so a
        // Critical setup/query failure cannot win the await and briefly make
        // an unhealthy participant visible.
        let ready = match &session {
            #[cfg(feature = "test-harness")]
            BusLease::Borrowed => {
                if shutdown.is_requested() {
                    if let Some(queries) = queries.take() {
                        queries.close();
                    }
                    return startup_teardown(
                        managed_tasks,
                        &participant,
                        &api,
                        &mut state,
                        shutdown_grace,
                        Ok(()),
                        session,
                    )
                    .await;
                }
                None
            }
            BusLease::Owned(owner) => Some(tokio::select! {
                biased;
                _ = shutdown.wait() => {
                    if let Some(queries) = queries.take() {
                        queries.close();
                    }
                    return startup_teardown(
                        managed_tasks,
                        &participant,
                        &api,
                        &mut state,
                        shutdown_grace,
                        Ok(()),
                        session,
                    ).await;
                }
                fault = bus.wait_for_fatal() => {
                    if let Some(queries) = queries.take() {
                        queries.close();
                    }
                    return startup_teardown(
                        managed_tasks,
                        &participant,
                        &api,
                        &mut state,
                        shutdown_grace,
                        Err(ParticipantFault::Bus(fault).into()),
                        session,
                    ).await;
                }
                exit = managed_tasks.next_unexpected_exit() => {
                    if let Some(queries) = queries.take() {
                        queries.close();
                    }
                    return startup_teardown(
                        managed_tasks,
                        &participant,
                        &api,
                        &mut state,
                        shutdown_grace,
                        Err(exit.into()),
                        session,
                    ).await;
                }
                result = owner.declare_participant_ready() => match result {
                    Ok(token) => token,
                    Err(error) => {
                        if let Some(queries) = queries.take() {
                            queries.close();
                        }
                        return startup_teardown(
                            managed_tasks,
                            &participant,
                            &api,
                            &mut state,
                            shutdown_grace,
                            Err(error.into()),
                            session,
                        ).await;
                    }
                },
            }),
        };

        tracing::info!(
            target: "phoxal.runtime",
            id = R::ID,
            participant = %participant_id,
            "runtime ready"
        );
        StartOutcome::Ready(Self {
            participant,
            api,
            state,
            bus: bus.clone(),
            session,
            clock,
            scheduler,
            schedule,
            clock_mode,
            timeline_retentions,
            queries,
            // Portable runtime evidence is measured at runner-owned step and
            // buffer boundaries. No OS sampler or participant telemetry is
            // involved.
            runtime_performance_publisher: RuntimePerformancePublisher::attach(bus),
            runtime_performance: RuntimePerformance::new(schedule),
            managed_tasks,
            ready,
            shutdown_grace,
        })
    }

    /// Drive the participant until it stops, then wind it down. Teardown runs
    /// before the exit is turned into a result, so hardware is parked whether
    /// the loop ended by request or by fault.
    pub(crate) async fn run<S>(mut self, shutdown: &mut ShutdownController<S>) -> crate::Result<()>
    where
        S: Future<Output = ()>,
    {
        let exit = self.main_loop(shutdown).await;
        let primary = exit.into_result();
        let report = self.finish().await;
        combine(primary, report)
    }

    async fn finish(self) -> TeardownReport {
        let Self {
            participant,
            api,
            mut state,
            queries,
            managed_tasks,
            session,
            ready,
            shutdown_grace,
            ..
        } = self;

        // Ready is revoked first: teardown must never leave a live lease while
        // participant resources are being unwound.
        drop(ready);

        // Query receive tasks stop next: nothing after this point serves a
        // request, and one arriving mid-teardown must not reach state the
        // shutdown hook is already unwinding.
        if let Some(queries) = queries {
            queries.close();
        }
        let deadline = ShutdownDeadline::from_now(shutdown_grace);
        let mut report = Teardown {
            managed_tasks,
            deadline,
        }
        .run(&participant, &api, &mut state)
        .await;
        let close_report = close_session(session, deadline).await;
        report.bus_close = close_report.bus_close;
        report
    }
}

async fn close_session(session: BusLease, deadline: ShutdownDeadline) -> TeardownReport {
    #[cfg(feature = "test-harness")]
    let BusLease::Owned(owner) = session else {
        return TeardownReport::default();
    };
    #[cfg(not(feature = "test-harness"))]
    let BusLease::Owned(owner) = session;

    let close = owner.close_until(deadline.instant()).await;
    if close.is_clean() {
        TeardownReport::default()
    } else {
        TeardownReport {
            bus_close: Some(close),
            ..TeardownReport::default()
        }
    }
}

/// Close the production owner, if this terminal path still has one, while
/// retaining its report alongside the primary result.
pub(crate) async fn close_session_with_result<T>(
    primary: crate::Result<T>,
    session: BusLease,
    deadline: ShutdownDeadline,
) -> crate::Result<T> {
    combine(primary, close_session(session, deadline).await)
}

/// Subscribe and feed the authoritative simulation clock. This task is
/// registered before setup and is cancelled through the ordinary teardown
/// sequence, so logical time cannot advance behind the runner's back.
async fn simulation_clock_feed(bus: BusHandle, handle: SimulationClockHandle) -> crate::Result<()> {
    super::event_loop::simulation_clock_feed(bus, handle).await
}
