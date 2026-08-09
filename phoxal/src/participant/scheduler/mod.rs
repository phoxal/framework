//! Step cadence: what a `#[phoxal::step(hz = …)]` loop declares, and what
//! releases each of its ticks.
//!
//! "Time reading" and "tick release" are separate seams.
//! [`ClockSource`](crate::participant::clock::ClockSource) answers "what time is
//! it", and every produced instant is read from it. [`StepScheduler`]
//! answers a different question: "when should the next participant step fire".
//! Real mode answers that from the host monotonic clock, never from a bus
//! message; a simulation clock instead releases ticks only when the world
//! authority advances robot time. Without this split, simulated time could
//! label samples but could never drive the loop - the runner would still
//! free-run on the host clock underneath a "simulated" label.
//!
//! Two schedulers ship:
//!
//! - [`RealScheduler`](real::RealScheduler) wraps Tokio wall-clock time.
//! - [`SimulationScheduler`](simulation::SimulationScheduler) is advanced by an
//!   external logical-time feed over a [`tokio::sync::watch`] channel (see the
//!   struct docs).
//!
//! Nothing here hard-codes real and simulation as the only possibilities: a
//! replay scheduler would be a third implementation of the same trait, not a
//! policy flag on an existing one.

use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::watch;

use crate::bus::RobotInstant;
use phoxal_bundle::ParticipantClock;

pub(crate) mod real;
pub(crate) mod simulation;

use real::RealScheduler;
use simulation::{SimulationClockHandle, SimulationScheduler};

/// The cadence of a `#[phoxal::step(hz = …)]` loop, as the role macros emit it
/// from the attribute.
#[derive(Clone, Copy, Debug)]
pub struct StepSchedule {
    /// Target frequency in Hz. Private because [`Self::period`] is the only
    /// honest reading of it: the raw number can name a period no scheduler can
    /// run.
    hz: f64,
}

impl StepSchedule {
    /// A schedule at `hz`.
    pub const fn hz(hz: f64) -> Self {
        StepSchedule { hz }
    }

    /// The nominal step period.
    ///
    /// Clamped to one nanosecond: a period of zero would make the scheduler
    /// release ticks in an unbounded spin instead of on a cadence, so an
    /// absurdly high `hz` degrades to "as fast as the loop can run" rather
    /// than to a busy loop with no yield point.
    pub(crate) fn period(&self) -> Duration {
        std::cmp::max(
            Duration::from_secs_f64(1.0 / self.hz),
            Duration::from_nanos(1),
        )
    }
}

/// The outcome of one [`StepScheduler::wait_until`] resolution.
///
/// The scheduler - not the runner - owns how an overrun is accounted for,
/// because "when does the next tick fire" and "how many ticks were skipped to
/// get there" are the same decision: a real scheduler measures overrun against
/// wall time, while a simulation scheduler measures it against however far the
/// external logical-time feed jumped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SchedulerTick {
    /// The robot instant the tick fired at (`>= target`, modulo the
    /// scheduler's own clamping/collapse policy).
    pub(crate) fired_at: RobotInstant,
    /// How many additional ticks were collapsed into this one after an overrun
    /// (0 when the tick fired on time). An overrun always collapses: one step
    /// runs and reports how many periods were skipped to reach it, and the
    /// skipped periods are never replayed as separate steps, so a stalled
    /// participant resumes at the cadence instead of in a catch-up storm.
    pub(crate) missed_ticks: u32,
}

/// Answers "when should the next `Participant::step` tick fire", given the
/// active clock mode. See the module docs for why this is a separate seam from
/// [`ClockSource`](crate::participant::clock::ClockSource).
///
/// Async methods (no `async_trait`): the runner awaits `wait_until` directly
/// inside its own `tokio::select!`, so boxing the future would be pure
/// overhead.
pub(crate) trait StepScheduler: Send + Sync + 'static {
    /// Wait until the tick logically due at `target` should release, collapsing
    /// any overrun, and report what actually happened.
    async fn wait_until(&self, target: RobotInstant) -> SchedulerTick;

    /// The scheduler's own view of "now", on the same timeline as
    /// [`SchedulerTick::fired_at`]. For [`RealScheduler`] this tracks the host
    /// clock; for [`SimulationScheduler`] it is the last instant observed from
    /// the feed, or `None` before any world history exists.
    fn now(&self) -> Option<RobotInstant>;
}

/// The runner's concrete scheduler choice for the active
/// [`ParticipantClock`](phoxal_bundle::ParticipantClock):
/// [`ParticipantClock::Real`](phoxal_bundle::ParticipantClock::Real) selects
/// [`RealScheduler`],
/// [`ParticipantClock::Simulation`](phoxal_bundle::ParticipantClock::Simulation)
/// selects [`SimulationScheduler`]. A single non-generic type lets the runner
/// hold "the scheduler" without threading a third generic parameter through
/// the runner alongside `R`/`C`; a replay scheduler would add a
/// variant here rather than change any call site.
pub(crate) enum AnyStepScheduler {
    /// Wall-clock scheduling (the default).
    Real(RealScheduler),
    /// Logical-time scheduling, driven by a
    /// [`SimulationClockHandle`](simulation::SimulationClockHandle).
    Simulation(SimulationScheduler),
    /// No scheduling at all: a participant outside robot time.
    ///
    /// Tools and the externally driven simulation controller have no
    /// `Participant::step` (the authoring macros reject one) and no robot time
    /// to schedule it against. This releases no tick ever, rather than
    /// pretending to run a cadence nobody drives.
    Disabled,
}

impl AnyStepScheduler {
    /// Validate scheduler facts without allocating a live scheduler or a
    /// simulation clock channel.
    ///
    /// Startup uses this pure check before transport exists. The lifecycle
    /// calls [`Self::for_clock_mode`] only after the bus connection succeeds,
    /// so simulation's watch channel and real mode's timer anchors are never
    /// constructed and discarded during local preflight.
    pub(crate) fn validate_clock_mode(
        clock_mode: ParticipantClock,
        schedule: Option<StepSchedule>,
        now: Option<RobotInstant>,
    ) -> crate::Result<()> {
        // Evaluate the period with the same validation boundary as live
        // construction, but retain no scheduler state here.
        let _period = schedule.map(|schedule| schedule.period());
        match clock_mode {
            ParticipantClock::Real if schedule.is_none() => Ok(()),
            ParticipantClock::Real => {
                now.context(
                    "a real participant cannot anchor its cadence without a synchronized clock",
                )?;
                Ok(())
            }
            ParticipantClock::Simulation => Ok(()),
            ParticipantClock::Clockless if schedule.is_some() => {
                anyhow::bail!("a participant with a step schedule cannot use clockless mode")
            }
            ParticipantClock::Clockless => Ok(()),
        }
    }

    /// The scheduler `clock_mode` calls for, plus the driving handle a
    /// simulation scheduler needs.
    ///
    /// This is the seam that answers "when should the next `Participant::step`
    /// tick fire", separate from the
    /// [`ClockSource`](crate::participant::clock::ClockSource) used for
    /// timestamps.
    ///
    /// Real mode returns no driving handle: its cadence comes from the host
    /// clock and nothing external feeds it. Simulation mode returns
    /// [`Some`] handle, which is the attachment point anything producing a
    /// [`RobotInstant`] drives the scheduler through - the runner's live
    /// `runtime/simulation/clock` subscription, a test, a REPL.
    pub(crate) fn for_clock_mode(
        clock_mode: ParticipantClock,
        schedule: Option<StepSchedule>,
        now: Option<RobotInstant>,
    ) -> crate::Result<(Self, Option<SimulationClockHandle>)> {
        Self::validate_clock_mode(clock_mode, schedule, now)?;
        let period = schedule.map(|schedule| schedule.period());
        Ok(match clock_mode {
            ParticipantClock::Real if schedule.is_none() => (AnyStepScheduler::Disabled, None),
            ParticipantClock::Real => {
                // A real participant has no instant to anchor its cadence on
                // until the clock is trustworthy. Anchoring on an invented
                // timeline would publish a world history nobody authored, so the
                // participant does not start at all - which is the ordinary
                // failure the supervisor already knows how to handle.
                let now = now.ok_or_else(|| {
                    anyhow::anyhow!(
                        "a real participant cannot anchor its cadence without a synchronized clock"
                    )
                })?;
                (
                    AnyStepScheduler::Real(
                        RealScheduler::new(period, now)
                            .context("the host boot clock could not be read to anchor cadence")?,
                    ),
                    None,
                )
            }
            ParticipantClock::Simulation => {
                // No seed at all: there is no world history until the authority
                // publishes one, and instant zero of an invented timeline would
                // be a world nobody authored.
                let (scheduler, handle) = SimulationScheduler::new(period);
                (AnyStepScheduler::Simulation(scheduler), Some(handle))
            }
            // A clockless participant has no cadence and no clock feed to
            // subscribe: it is driven by host events or by the simulator that
            // owns it, and it expresses no robot time.
            ParticipantClock::Clockless => (AnyStepScheduler::Disabled, None),
        })
    }

    /// Subscribe to logical-time changes when this is a simulation scheduler.
    ///
    /// The runner uses this independently of the optional step cadence so
    /// timeline replacement is still observed by clocked, step-less services.
    pub(crate) fn simulation_time_receiver(&self) -> Option<watch::Receiver<Option<RobotInstant>>> {
        match self {
            AnyStepScheduler::Real(_) | AnyStepScheduler::Disabled => None,
            AnyStepScheduler::Simulation(scheduler) => Some(scheduler.time_receiver()),
        }
    }

    /// Resolve when the tick due at `target` should release, and never resolve
    /// when there is no target - so a participant with no step schedule is
    /// driven by server queries and shutdown alone.
    ///
    /// This is the sole seam through which the runner asks "when should the next
    /// `Participant::step` tick fire": real mode sleeps on wall time, simulation
    /// mode waits on logical time, and the runner does not know which.
    pub(crate) async fn wait_until_due(&self, target: Option<RobotInstant>) -> SchedulerTick {
        match target {
            Some(target) => self.wait_until(target).await,
            None => std::future::pending().await,
        }
    }
}

impl StepScheduler for AnyStepScheduler {
    async fn wait_until(&self, target: RobotInstant) -> SchedulerTick {
        match self {
            AnyStepScheduler::Real(scheduler) => scheduler.wait_until(target).await,
            AnyStepScheduler::Simulation(scheduler) => scheduler.wait_until(target).await,
            AnyStepScheduler::Disabled => std::future::pending().await,
        }
    }

    fn now(&self) -> Option<RobotInstant> {
        match self {
            AnyStepScheduler::Real(scheduler) => scheduler.now(),
            AnyStepScheduler::Simulation(scheduler) => scheduler.now(),
            AnyStepScheduler::Disabled => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::TimelineId;
    use crate::participant::duration_nanos;

    fn at(timeline: u64, ticks: u64) -> RobotInstant {
        RobotInstant::new(
            TimelineId::from_raw(timeline).expect("test timeline must be nonzero"),
            ticks,
        )
    }

    #[test]
    fn step_schedule_period_never_rounds_to_zero() {
        assert_eq!(StepSchedule::hz(f64::MAX).period(), Duration::from_nanos(1));
    }

    #[test]
    fn the_clock_mode_selects_the_real_or_simulation_scheduler() {
        let schedule = Some(StepSchedule::hz(100.0));

        let (real, real_handle) =
            AnyStepScheduler::for_clock_mode(ParticipantClock::Real, schedule, Some(at(1, 0)))
                .expect("real scheduler");
        assert!(matches!(real, AnyStepScheduler::Real(_)));
        assert!(
            real_handle.is_none(),
            "real mode has no simulation clock handle to drive"
        );

        let (simulation, simulation_handle) =
            AnyStepScheduler::for_clock_mode(ParticipantClock::Simulation, schedule, None)
                .expect("simulation scheduler");
        assert!(matches!(simulation, AnyStepScheduler::Simulation(_)));
        assert!(
            simulation_handle.is_some(),
            "simulation mode must hand back the driving handle so the caller can wire the live feed"
        );
        assert_eq!(
            simulation.now(),
            None,
            "simulation mode starts with no world history, not with an invented zero"
        );
    }

    #[test]
    fn a_real_participant_without_a_step_schedule_allocates_no_step_scheduler() {
        let (scheduler, handle) =
            AnyStepScheduler::for_clock_mode(ParticipantClock::Real, None, Some(at(1, 0)))
                .expect("runner clock");
        assert!(matches!(scheduler, AnyStepScheduler::Disabled));
        assert!(handle.is_none());
    }

    #[test]
    fn a_clockless_participant_cannot_silently_disable_its_declared_step() {
        let error = AnyStepScheduler::for_clock_mode(
            ParticipantClock::Clockless,
            Some(StepSchedule::hz(50.0)),
            None,
        )
        .err()
        .expect("clockless mode cannot drive a scheduled transition");
        assert!(error.to_string().contains("step schedule"));
    }

    #[test]
    fn a_real_participant_with_no_trustworthy_clock_does_not_get_a_cadence_at_all() {
        // The alternative was anchoring the cadence on an invented timeline at
        // tick zero, which publishes a world history nobody authored. Refusing
        // to start is the ordinary failure the supervisor already handles.
        assert!(
            AnyStepScheduler::for_clock_mode(
                ParticipantClock::Real,
                Some(StepSchedule::hz(50.0)),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn preflight_scheduler_validation_does_not_construct_a_live_scheduler() {
        AnyStepScheduler::validate_clock_mode(
            ParticipantClock::Simulation,
            Some(StepSchedule::hz(20.0)),
            None,
        )
        .expect("simulation facts do not need a seed instant or a live channel");
        AnyStepScheduler::validate_clock_mode(ParticipantClock::Clockless, None, None)
            .expect("clockless facts are valid without a scheduler");
        assert!(
            AnyStepScheduler::validate_clock_mode(
                ParticipantClock::Clockless,
                Some(StepSchedule::hz(20.0)),
                None,
            )
            .is_err()
        );
        assert!(
            AnyStepScheduler::validate_clock_mode(
                ParticipantClock::Real,
                Some(StepSchedule::hz(20.0)),
                None,
            )
            .is_err()
        );
    }

    /// The exact scheduler + handle `for_clock_mode` selects for
    /// [`ParticipantClock::Simulation`], driven purely by robot time - no real
    /// sleeping, no live bus/Webots feed. This is the deterministic proof that
    /// simulation mode schedules ticks from robot time; the full live path (the
    /// clock feed wiring and the wire-key match with the simulation
    /// controller's publisher) needs a bus and belongs to the local end-to-end
    /// run.
    #[tokio::test]
    async fn the_simulation_scheduler_the_runner_selects_schedules_deterministically() {
        let schedule = StepSchedule::hz(10.0); // 100ms period
        let period_ns = duration_nanos(schedule.period());

        let (scheduler, handle) =
            AnyStepScheduler::for_clock_mode(ParticipantClock::Simulation, Some(schedule), None)
                .expect("simulation scheduler");
        let handle = handle.expect("simulation mode must hand back a driving handle");

        let mut fired = Vec::new();
        let mut target = at(1, period_ns);
        for _ in 0..3 {
            handle.advance(target);
            let tick = scheduler.wait_until(target).await;
            fired.push(tick.fired_at.ticks());
            target = at(1, target.ticks() + period_ns);
        }

        assert_eq!(
            fired,
            vec![100_000_000, 200_000_000, 300_000_000],
            "ticks fire in order at the instants the handle advanced to, with no real sleeping"
        );
    }
}
