//! The serialized participant scheduler and event loop.

use std::time::Duration;

use crate::api;
use crate::bus::{LocalInstant, RobotInstant, StepToken, Subscriber, TimelineId};
use crate::participant::api::Participant;
use crate::participant::clock::{ClockReading, ClockSource, TimeUnsynchronized};
use crate::participant::context::{ResetContext, StepContext, TimelineRetention};
use crate::participant::scheduler::simulation::{SimulationClockAdvance, SimulationClockHandle};
use crate::participant::scheduler::{SchedulerTick, StepScheduler};
use phoxal_runtime_contract::launch::ClockMode;

use super::ShutdownController;
use super::lifecycle::{LoopExit, Runner};
use super::query::QuerySurface;

/// How often the runner wakes for work that is not a step: publishing the
/// runtime-performance rollup, and re-checking clock discipline.
const RUNTIME_PERFORMANCE_TICK_INTERVAL: Duration = Duration::from_secs(1);

impl<R: Participant, C: ClockSource> Runner<R, C> {
    pub(crate) async fn main_loop<S>(&mut self, shutdown: &mut ShutdownController<S>) -> LoopExit
    where
        S: std::future::Future<Output = ()>,
    {
        let period = self.schedule.map(|schedule| schedule.period());
        let mut step_index: u64 = 0;
        let mut active_timeline: Option<TimelineId> = None;
        let mut simulation_time_rx = self.scheduler.simulation_time_receiver();
        // The simulation clock feed starts before `Participant::setup`. If setup
        // takes long enough for the authority's first world step to arrive, a
        // newly-cloned watch receiver sees that value as its initial state and
        // has no change notification to deliver. Establish that already-current
        // world history without invoking reset: there was no prior participant
        // execution, but its ingress barrier and first cadence still matter.
        let initial_time = self.scheduler.now();
        if let Some(initial_time) = initial_time.filter(|_| simulation_time_rx.is_some()) {
            active_timeline = Some(initial_time.timeline());
            retain_timeline(&self.timeline_retentions, initial_time.timeline());
        }
        let mut last_step_at = initial_time;
        // The next tick's *robot* due time - what the runner asks the scheduler
        // to release at, separate from the host-monotonic beat below.
        let mut next_step_target =
            initial_time.and_then(|at| period.map(|period| advance_step_deadline(at, period, 0)));
        let mut beat = tokio::time::interval_at(
            tokio::time::Instant::now(),
            RUNTIME_PERFORMANCE_TICK_INTERVAL,
        );
        beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // Order matters: shutdown first, then a managed-task fault (both are
                // "stop the loop" events and should preempt routine work), then the
                // runtime-performance publication tick, then a *due* step, then
                // server queries. Publication is cheap and must not be starved by
                // an overloaded participant; due steps still take priority over a
                // steady query backlog.
                biased;
                _ = shutdown.wait() => return LoopExit::ShutdownRequested,
                exit = self.managed_tasks.next_unexpected_exit() => {
                    tracing::error!(
                        target: "phoxal.runtime",
                        task = %exit.name,
                        failure = %exit,
                        "managed task exited unexpectedly; faulting the participant"
                    );
                    return LoopExit::ManagedTaskFaulted(exit);
                }
                fired_at = simulation_time_change(&mut simulation_time_rx) => {
                    if active_timeline == Some(fired_at.timeline()) {
                        continue;
                    }

                    // Timelines are opaque identities, not ordered generations. Any
                    // different one establishes a replacement world history. This
                    // branch is independent of `Participant::step`, so clocked server-only
                    // services receive the same serialized reset lifecycle.
                    let previous_timeline = active_timeline.replace(fired_at.timeline());
                    retain_timeline(&self.timeline_retentions, fired_at.timeline());
                    if let Some(previous_timeline) = previous_timeline {
                        let reset = ResetContext {
                            previous_timeline,
                            new_timeline: fired_at.timeline(),
                        };
                        if let Err(error) = self
                            .participant
                            .reset(reset, &self.api, &mut self.state)
                        {
                            return LoopExit::ResetFailed(error);
                        }
                    }
                    next_step_target =
                        period.map(|period| advance_step_deadline(fired_at, period, 0));
                    step_index = 0;
                    last_step_at = Some(fired_at);
                    self.runtime_performance.reset(self.schedule);
                }
                _ = beat.tick() => {
                    // A real participant with no `Participant::step` schedule would otherwise
                    // check its clock once at startup and never again, and go on
                    // serving queries from state it cannot date. This beat
                    // is its only recurring one, so clock discipline is checked
                    // here too - a stepping participant reaches the same check
                    // sooner, in its own step arm.
                    //
                    // Simulation is excluded on purpose: there, "unsynchronized"
                    // means the world authority has not published a first step yet,
                    // which is a world that has not started rather than a clock
                    // that was lost.
                    let faulted = LocalInstant::clock_faulted()
                        .then_some(TimeUnsynchronized::ClockFault)
                        .or_else(|| match (period, self.clock_mode) {
                            (None, ClockMode::Real) => match self.clock.read() {
                                ClockReading::Unsynchronized(reason) => Some(reason),
                                ClockReading::Synchronized(_) => None,
                            },
                            _ => None,
                        });
                    if let Some(reason) = faulted {
                        tracing::error!(
                            target: "phoxal.runtime",
                            error = %reason,
                            "clock discipline lost; failing the participant"
                        );
                        return LoopExit::ClockDisciplineLost(reason);
                    }
                    if let Some(rollup) = self.runtime_performance.take_rollup(&self.bus) {
                        self.runtime_performance_publisher.publish(rollup);
                    }
                }
                SchedulerTick { fired_at, missed_ticks }
                    = self.scheduler.wait_until_due(next_step_target) =>
                {
                    let (Some(period), Some(target)) = (period, next_step_target) else { continue };

                    // A boot-clock read failed somewhere in this process - the bus
                    // stamper, a driver's permit, an arbiter's silence deadline.
                    // Each of those failed closed on its own, but a process that
                    // cannot read its own clock does not get to carry on once reads
                    // start working again: recovery is a fresh process.
                    if LocalInstant::clock_faulted() {
                        tracing::error!(
                            target: "phoxal.runtime",
                            error = %TimeUnsynchronized::ClockFault,
                            "clock discipline lost; failing the participant"
                        );
                        return LoopExit::ClockDisciplineLost(TimeUnsynchronized::ClockFault);
                    }

                    if fired_at.timeline() != target.timeline() {
                        // The independent simulation-time branch above owns
                        // timeline replacement. A simultaneously-ready watch
                        // notification is biased ahead of this branch; this is only
                        // defensive against a future scheduler implementation.
                        next_step_target = Some(advance_step_deadline(fired_at, period, 0));
                        continue;
                    }
                    active_timeline.get_or_insert(fired_at.timeline());
                    next_step_target = Some(advance_step_deadline(target, period, missed_ticks));

                    let now = match self.clock.read() {
                        ClockReading::Synchronized(now) if now.timeline() == target.timeline() => now,
                        ClockReading::Synchronized(_) => {
                            // The clock feed can replace the world history after the
                            // scheduler resolves but before this read. Let the
                            // higher-priority simulation-time arm install the
                            // ingress barrier and run Participant::reset before any step on
                            // the new timeline.
                            continue;
                        }
                        ClockReading::Unsynchronized(reason) => {
                            // Do not freeze, and do not hold on hoping it comes
                            // back: a frozen participant is what leaves an actuator
                            // commanded, and there is no uncertainty estimator that
                            // could justify a grace window. The participant fails
                            // now, teardown parks the hardware, and supervisor's
                            // ordinary restart policy decides what happens next.
                            tracing::error!(
                                target: "phoxal.runtime",
                                error = %reason,
                                "clock discipline lost; failing the participant"
                            );
                            return LoopExit::ClockDisciplineLost(reason);
                        }
                    };
                    let dt = last_step_at
                        .and_then(|last| now.duration_since(last).ok())
                        .unwrap_or_default();
                    last_step_at = Some(now);

                    let step = StepContext {
                        token: StepToken::mint(now),
                        step_index,
                        dt,
                        missed_ticks,
                    };
                    step_index += 1;

                    // A handler error is terminal. A scheduled transition owns
                    // the participant's mutable state, so continuing after an
                    // error would make the Ready claim untrustworthy.
                    let observation =
                        self.runtime_performance
                            .begin_step(target, fired_at, missed_ticks);
                    let success = match self.participant.step(&self.api, step, &mut self.state) {
                        Ok(()) => true,
                        Err(e) => {
                            self.runtime_performance.finish_step(observation, false);
                            return LoopExit::StepFailed(e);
                        }
                    };
                    self.runtime_performance.finish_step(observation, success);
                }
                request = next_query(&mut self.queries) => {
                    if let Err(error) = self.serve_query(request) {
                        return LoopExit::QueryDispatchFailed(error);
                    }
                }
            }
        }
    }

    fn serve_query(&mut self, request: (usize, phoxal_bus::IncomingQuery)) -> crate::Result<()> {
        let Some(queries) = &self.queries else {
            return Ok(());
        };
        queries.serve(request, &self.participant, &self.api, &mut self.state)
    }
}

/// Subscribe the authoritative `simulation/clock` feed and drive the live
/// scheduler from exact production instants for the task's lifetime.
pub(crate) async fn simulation_clock_feed(
    bus: phoxal_bus::BusHandle,
    handle: SimulationClockHandle,
) -> crate::Result<()> {
    let topic = api::topic::client().simulation().clock();
    let subscriber = match Subscriber::<api::simulation::Clock>::new(&bus, &topic).await {
        Ok(subscriber) => subscriber,
        Err(error) => return Err(error.into()),
    };
    tracing::info!(
        target: "phoxal.runtime",
        topic = topic.key(),
        "subscribed the live simulation/clock feed; driving the simulation scheduler from it"
    );
    loop {
        let observed = subscriber
            .recv()
            .await
            .map_err(|error| anyhow::anyhow!("simulation/clock subscriber terminated: {error}"))?;
        let Some(at) = observed.metadata.produced_exactly_at() else {
            return Err(anyhow::anyhow!(
                "simulation/clock sample has no exact production instant"
            ));
        };
        match handle.advance(at) {
            SimulationClockAdvance::Advanced | SimulationClockAdvance::DuplicateOrBackward => {}
            SimulationClockAdvance::RetiredTimeline => {
                tracing::warn!(
                    target: "phoxal.runtime",
                    timeline = %at.timeline(),
                    ticks = at.ticks(),
                    "ignoring late simulation clock from a retired world history"
                );
            }
        }
    }
}

/// Resolve on the next request when a query surface exists, and never when it
/// does not.
async fn next_query<R: Participant>(
    queries: &mut Option<QuerySurface<R>>,
) -> (usize, phoxal_bus::IncomingQuery) {
    match queries {
        Some(queries) => queries.next_request().await,
        None => std::future::pending().await,
    }
}

/// Resolve on the next logical-time change when this participant observes one,
/// and never when it does not.
async fn simulation_time_change(
    receiver: &mut Option<tokio::sync::watch::Receiver<Option<RobotInstant>>>,
) -> RobotInstant {
    let Some(receiver) = receiver else {
        return std::future::pending().await;
    };
    loop {
        if receiver.changed().await.is_ok() {
            if let Some(at) = *receiver.borrow_and_update() {
                return at;
            }
            continue;
        }
        // A simulation scheduler retains its sender for the runner lifetime.
        // If a future implementation closes it, disable this branch instead
        // of spinning.
        std::future::pending::<()>().await;
    }
}

/// The instant the step after the one due at `target` is due at: one period on,
/// plus one for each period a released tick collapsed.
pub(crate) fn advance_step_deadline(
    target: RobotInstant,
    period: Duration,
    missed_ticks: u32,
) -> RobotInstant {
    target.saturating_add(period.saturating_mul(missed_ticks.saturating_add(1)))
}

pub(crate) fn retain_timeline(retentions: &[TimelineRetention], timeline: TimelineId) {
    for retention in retentions {
        retention(timeline);
    }
}
