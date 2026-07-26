//! The step scheduler (D34/#09): split "time reading" from "tick release".
//!
//! [`ClockSource`](crate::participant::clock::ClockSource) answers "what time is
//! it", and every produced instant is read from it. [`StepScheduler`]
//! answers a different question: "when should the next `#[step]` tick fire".
//! Real mode answers that from the host monotonic clock, never from a bus
//! message; a simulation clock instead releases ticks only when the world
//! authority advances robot time. Without this split, simulated time could
//! label samples but could never drive the loop - the runner would still
//! free-run on the host clock underneath a "simulated" label.
//!
//! Three schedulers are shipped or scoped here:
//!
//! - [`RealScheduler`] wraps Tokio wall-clock time and preserves the runner's
//!   existing cadence/collapse behavior exactly (D34).
//! - [`SimulationScheduler`] is advanced by an external logical-time feed - a
//!   [`tokio::sync::watch`] handle in this slice, the seam a future bus
//!   subscription to `simulation/clock` would push into (see the struct docs).
//! - A replay scheduler is left to a future slice (out of scope here); nothing
//!   below hard-codes real+simulation as the only two variants, but only those
//!   two ship.

use std::time::Duration;

use tokio::sync::watch;

use crate::bus::{LocalInstant, RobotInstant, TimelineId};
use crate::participant::spec::MissedTick;
use phoxal_bus::RetiredTimelines;

pub(crate) fn duration_to_nanos_saturating(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// The outcome of one [`StepScheduler::wait_until`] resolution.
///
/// The scheduler - not the runner - owns the missed-tick/catch-up policy,
/// because "when does the next tick fire" and "how many ticks were skipped to
/// get there" are the same decision (D34): a real scheduler measures overrun
/// against wall time, while a simulation scheduler measures it against however
/// far the external logical-time feed jumped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerTick {
    /// The robot instant the tick fired at (`>= target`, modulo the
    /// scheduler's own clamping/collapse policy).
    pub fired_at: RobotInstant,
    /// How many additional ticks were collapsed into this one after an
    /// overrun (0 when the tick fired on time). Under
    /// [`MissedTick::Collapse`] this is "how many periods were skipped", never
    /// replayed as separate steps (no catch-up storm).
    pub missed_ticks: u32,
}

/// Answers "when should the next `#[step]` tick fire", given the active clock
/// mode. See the module docs for why this is a separate seam from
/// [`ClockSource`](crate::participant::clock::ClockSource).
///
/// Async methods (no `async_trait`) to match the runner's existing async
/// style (`ParticipantLifecycle`, D34): the runner awaits `wait_until` directly
/// inside its own `tokio::select!`, so boxing the future would be pure
/// overhead.
#[allow(async_fn_in_trait)]
pub trait StepScheduler: Send + Sync + 'static {
    /// Wait until the tick logically due at `target` should release, applying
    /// the scheduler's missed-tick policy, and report what actually happened.
    async fn wait_until(&self, target: RobotInstant) -> SchedulerTick;

    /// The scheduler's own view of "now", on the same timeline as
    /// [`SchedulerTick::fired_at`]. For [`RealScheduler`] this tracks the host
    /// clock; for [`SimulationScheduler`] it is the last instant observed from
    /// the feed, or `None` before any world history exists.
    fn now(&self) -> Option<RobotInstant>;
}

/// Wall-clock scheduler: wraps [`tokio::time::Instant`] sleeps. This is the
/// default and must reproduce the runner's pre-#09 cadence/collapse behavior
/// exactly - real mode is not allowed to regress.
///
/// A [`RobotInstant`] carries no wall-clock unit of its own, so
/// [`RealScheduler`] sleeps on Tokio's timer but never *reads* time from it:
/// the timer decides when to wake, and the host's suspend-aware boot clock
/// ([`LocalInstant`]) decides what instant was actually reached. A host that
/// suspends for an hour therefore resumes with an hour of missed periods, not
/// with an hour that never happened. Real cadence never waits on a bus message
/// (#952 section I).
pub struct RealScheduler {
    missed_tick: MissedTick,
    /// The nominal step period, needed to collapse a multi-period overrun
    /// into a single released tick (see [`Self::wait_until`]). `None` when the
    /// participant has no `#[step]` schedule at all.
    period: Option<Duration>,
    timeline: TimelineId,
    /// The boot-clock anchor every released tick is measured against. Sampled
    /// before `started_timer`, so the timer anchor is never the earlier of the
    /// two.
    started_boot: LocalInstant,
    /// The timer anchor. Tokio's timer runs on the *stopping* clock, so this
    /// decides only when to wake up, never what time it is.
    started_timer: tokio::time::Instant,
    started_ticks: u64,
}

impl RealScheduler {
    /// A real scheduler anchored to `now` (the instant the runner's clock
    /// reports at start), running `period` and applying `missed_tick` after an
    /// overrun. `period` is `None` for a step-less participant, in which case
    /// [`Self::wait_until`] is never called by the runner.
    /// `None` when the host boot clock cannot be read: without an anchor there
    /// is no cadence to run, which the caller reports as ordinary failure.
    pub fn new(
        missed_tick: MissedTick,
        period: Option<Duration>,
        now: RobotInstant,
    ) -> Option<Self> {
        // The boot anchor is sampled *first*, so the timer anchor is never
        // earlier than it. Sampling the other way round leaves the timer able
        // to reach its deadline while the boot clock has covered slightly less
        // ground, which would release a tick that reads as earlier than the
        // target it was released for.
        let started_boot = LocalInstant::try_now()?;
        Some(RealScheduler {
            missed_tick,
            period,
            timeline: now.timeline(),
            started_boot,
            started_timer: tokio::time::Instant::now(),
            started_ticks: now.ticks(),
        })
    }

    /// Convert an instant on this scheduler's timeline to the equivalent host
    /// timer deadline, anchored at construction.
    fn timer_deadline_for(&self, target: RobotInstant) -> tokio::time::Instant {
        let delta_ns = target.ticks().saturating_sub(self.started_ticks);
        self.started_timer + Duration::from_nanos(delta_ns)
    }

    /// How far the boot clock has moved since this scheduler was anchored, or
    /// `None` when the clock cannot be read at all.
    fn boot_elapsed(&self) -> Option<Duration> {
        Some(LocalInstant::try_now()?.saturating_duration_since(self.started_boot))
    }
}

/// Resolve a released tick from the boot clock, independent of the timer that
/// woke the task.
///
/// Splitting this out keeps the only arithmetic that matters testable without
/// a host suspend: `elapsed` is a boot-clock measurement, so a test supplies
/// the suspend directly.
///
/// There is no early-wake case to defend against. If the timer's clock stops
/// during suspend while the boot clock keeps counting, the wake is *late* in
/// boot terms; if both count, they move together; and while the host is awake a
/// monotonic timer cannot fire before its own deadline. So one boot-clock read
/// after the wake is enough - no re-sleep loop.
fn resolve_tick(
    started_ticks: u64,
    elapsed: Duration,
    target_ticks: u64,
    period: Option<Duration>,
    missed_tick: MissedTick,
) -> (u64, u32) {
    // A tick is never released before the instant it was released *for*: the
    // boot anchor precedes the timer anchor, so this clamp only ever absorbs
    // that construction skew.
    let fired_ticks = started_ticks
        .saturating_add(duration_to_nanos_saturating(elapsed))
        .max(target_ticks);
    // Collapse policy (D34, the v1 default): after an overrun, fire once
    // and record how many periods were skipped rather than replaying each
    // missed tick back-to-back (no catch-up storm). `CatchUp` is reserved
    // for offline replay and is not driven by this scheduler (#09 scope:
    // real + simulation only) - it resolves at `target` with no
    // collapsing, i.e. `missed_ticks` is always 0.
    //
    // The count is arithmetic rather than a period-by-period walk: an overrun
    // is unbounded in principle (a suspended host), and a loop over it is a
    // hang rather than a slow answer.
    let mut missed_ticks = 0u32;
    if missed_tick == MissedTick::Collapse
        && let Some(period) = period.filter(|period| !period.is_zero())
    {
        let period_ns = duration_to_nanos_saturating(period);
        let overrun = fired_ticks.saturating_sub(target_ticks);
        missed_ticks = u32::try_from(overrun / period_ns).unwrap_or(u32::MAX);
    }
    (fired_ticks, missed_ticks)
}

impl StepScheduler for RealScheduler {
    async fn wait_until(&self, target: RobotInstant) -> SchedulerTick {
        tokio::time::sleep_until(self.timer_deadline_for(target)).await;

        // A tick released while the boot clock is unreadable resolves at its
        // own target and reports no overrun: there is nothing to measure. The
        // runner reads the clock again before it builds the step context, so
        // the read failure surfaces there as lost clock discipline.
        let Some(elapsed) = self.boot_elapsed() else {
            return SchedulerTick {
                fired_at: target,
                missed_ticks: 0,
            };
        };
        let (fired_ticks, missed_ticks) = resolve_tick(
            self.started_ticks,
            elapsed,
            target.ticks(),
            self.period,
            self.missed_tick,
        );
        SchedulerTick {
            fired_at: RobotInstant::new(self.timeline, fired_ticks),
            missed_ticks,
        }
    }

    fn now(&self) -> Option<RobotInstant> {
        Some(RobotInstant::new(
            self.timeline,
            self.started_ticks
                .saturating_add(duration_to_nanos_saturating(self.boot_elapsed()?)),
        ))
    }
}

/// Simulation scheduler: releases ticks from **robot** time advanced by the
/// world authority, never a real sleep.
///
/// # The live seam
///
/// The simulation controller is the authoritative owner of the
/// `simulation/clock` state topic.
/// In simulation mode the participant runner subscribes that topic through
/// `spawn_simulation_clock_feed` and forwards each observed [`RobotInstant`]
/// into this scheduler through [`SimulationClockHandle::advance`].
/// Tests drive the same handle directly, so live and deterministic test paths
/// share the scheduler boundary.
///
/// # Determinism
///
/// [`SimulationScheduler::wait_until`] never sleeps on a wall-clock timer: it
/// awaits a [`tokio::sync::watch`] change, so a test drives robot time
/// forward with [`SimulationClockHandle::advance`] and gets deterministic tick
/// order with no real waiting. Clock silence is the only pause signal.
pub struct SimulationScheduler {
    missed_tick: MissedTick,
    /// The nominal step period, used to count how many whole periods a
    /// logical-time jump spanned (see [`Self::wait_until`]). `None` when the
    /// participant has no `#[step]` schedule.
    period: Option<Duration>,
    /// Keeps the watch channel open even when the runner has not wired an
    /// external `simulation/clock` feed yet. Without this, dropping the
    /// returned handle would close the channel and make waits resolve
    /// immediately instead of waiting for logical time.
    _tx_keepalive: watch::Sender<Option<RobotInstant>>,
    rx: watch::Receiver<Option<RobotInstant>>,
}

struct SimulationClockState {
    current: Option<RobotInstant>,
    retired_timelines: RetiredTimelines,
}

/// Result of applying one clock sample to a simulation scheduler.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimulationClockAdvance {
    Advanced,
    DuplicateOrBackward,
    RetiredTimeline,
}

/// A cloneable handle that advances the logical time a
/// [`SimulationScheduler`] observes.
///
/// This is the seam a live `simulation/clock` bus subscription attaches to
/// (see [`SimulationScheduler`] docs): a subscriber task would call
/// [`advance`](Self::advance) once per received sample. Tests use the same
/// method to drive the scheduler deterministically, with no bus and no real
/// sleeping.
#[derive(Clone)]
pub struct SimulationClockHandle {
    tx: watch::Sender<Option<RobotInstant>>,
    state: std::sync::Arc<std::sync::Mutex<SimulationClockState>>,
}

impl SimulationClockHandle {
    /// Advance the observed robot time to `at`. A no-op if `at` is a duplicate
    /// or backwards within the active timeline. Any different timeline replaces
    /// the active world history, since timelines are opaque identities with no
    /// generation order; recently retired timelines are ignored so an in-flight
    /// clock from a dead controller cannot reactivate old state.
    pub fn advance(&self, at: RobotInstant) -> SimulationClockAdvance {
        let mut state = self.state.lock().expect("simulation clock mutex poisoned");
        match state.current {
            Some(current) if current.timeline() == at.timeline() => {
                if at.ticks() <= current.ticks() {
                    return SimulationClockAdvance::DuplicateOrBackward;
                }
            }
            current => {
                if state.retired_timelines.contains(at.timeline()) {
                    return SimulationClockAdvance::RetiredTimeline;
                }
                if let Some(previous) = current {
                    state.retired_timelines.retire(previous.timeline());
                }
                state.retired_timelines.activate(at.timeline());
            }
        }
        state.current = Some(at);
        self.tx.send_replace(Some(at));
        SimulationClockAdvance::Advanced
    }
}

impl SimulationScheduler {
    /// Build a simulation scheduler starting at `start` and running `period`,
    /// plus its driving handle. `missed_tick` is the schedule's policy (D34);
    /// under [`MissedTick::Collapse`] a logical-time jump spanning multiple
    /// periods fires once and reports the skipped count, mirroring
    /// [`RealScheduler`]'s behavior for a wall-clock overrun. `period` is
    /// `None` for a step-less participant, in which case [`Self::wait_until`]
    /// is never called by the runner.
    pub fn new(missed_tick: MissedTick, period: Option<Duration>) -> (Self, SimulationClockHandle) {
        // There is no world history until the authority publishes one. Seeding
        // an invented instant here is exactly the `(0, 0)` sentinel this train
        // deletes.
        let (tx, rx) = watch::channel(None);
        let scheduler = SimulationScheduler {
            missed_tick,
            period,
            _tx_keepalive: tx.clone(),
            rx,
        };
        let handle = SimulationClockHandle {
            tx,
            state: std::sync::Arc::new(std::sync::Mutex::new(SimulationClockState {
                current: None,
                retired_timelines: RetiredTimelines::default(),
            })),
        };
        (scheduler, handle)
    }

    /// A [`SimulationClock`](crate::participant::clock::SimulationClock) that
    /// observes the same feed this scheduler releases ticks from. The runner
    /// uses it as the instant source in simulation mode so `#[step]` release
    /// time and stamped production time never diverge.
    pub(crate) fn simulation_clock(&self) -> crate::participant::clock::SimulationClock {
        crate::participant::clock::SimulationClock::from_receiver(self.rx.clone())
    }

    fn missed_ticks(&self, target: RobotInstant, current: RobotInstant) -> u32 {
        let (MissedTick::Collapse, Some(period)) = (self.missed_tick, self.period) else {
            return 0;
        };
        let Ok(overrun) = current.duration_since(target) else {
            return 0;
        };
        let period_ns = duration_to_nanos_saturating(period);
        if period_ns == 0 {
            return 0;
        }
        u32::try_from(duration_to_nanos_saturating(overrun) / period_ns).unwrap_or(u32::MAX)
    }
}

impl StepScheduler for SimulationScheduler {
    async fn wait_until(&self, target: RobotInstant) -> SchedulerTick {
        let mut rx = self.rx.clone();
        loop {
            if let Some(current) = *rx.borrow_and_update() {
                let reached = current.timeline() != target.timeline()
                    || current
                        .checked_cmp(target)
                        .is_ok_and(|order| order != std::cmp::Ordering::Less);
                if reached {
                    // Collapse policy (D34): when the feed had already advanced
                    // past `target` (by one or more whole periods) before we
                    // even started waiting, fire once and report how many
                    // periods were skipped, rather than the runner replaying
                    // each one - the same "no catch-up storm" behavior as
                    // `RealScheduler`'s overrun handling.
                    return SchedulerTick {
                        fired_at: current,
                        missed_ticks: self.missed_ticks(target, current),
                    };
                }
            }
            if rx.changed().await.is_err() {
                // `SimulationScheduler` owns a sender keepalive, so this is
                // only defensive for future constructors: never release a tick
                // for a world history that does not exist.
                return std::future::pending().await;
            }
        }
    }

    fn now(&self) -> Option<RobotInstant> {
        *self.rx.borrow()
    }
}

/// The runner's concrete scheduler choice for the active
/// [`ClockMode`](crate::participant::launch::ClockMode) (D34/#09):
/// [`ClockMode::Real`](crate::participant::launch::ClockMode::Real) selects
/// [`RealScheduler`],
/// [`ClockMode::Simulation`](crate::participant::launch::ClockMode::Simulation)
/// selects [`SimulationScheduler`]. A single non-generic type lets the runner
/// hold "the scheduler" without threading a third generic parameter through
/// `run_with`/`run_with_bus` alongside `R`/`C` (replay, if it ships later,
/// would add a third variant here rather than changing any call site).
pub enum AnyStepScheduler {
    /// Wall-clock scheduling (the default).
    Real(RealScheduler),
    /// Logical-time scheduling, driven by a [`SimulationClockHandle`].
    Simulation(SimulationScheduler),
    /// No scheduling at all: a participant outside robot time.
    ///
    /// Tools and the externally driven simulation controller have no `#[step]`
    /// (the authoring macros reject one) and no robot time to schedule it
    /// against. This releases no tick ever, rather than pretending to run a
    /// cadence nobody drives.
    Clockless,
}

impl AnyStepScheduler {
    /// Subscribe to logical-time changes when this is a simulation scheduler.
    ///
    /// The runner uses this independently of the optional step cadence so
    /// timeline replacement is still observed by clocked, step-less services.
    pub(crate) fn simulation_time_receiver(&self) -> Option<watch::Receiver<Option<RobotInstant>>> {
        match self {
            AnyStepScheduler::Real(_) | AnyStepScheduler::Clockless => None,
            AnyStepScheduler::Simulation(scheduler) => Some(scheduler.rx.clone()),
        }
    }
}

impl StepScheduler for AnyStepScheduler {
    async fn wait_until(&self, target: RobotInstant) -> SchedulerTick {
        match self {
            AnyStepScheduler::Real(scheduler) => scheduler.wait_until(target).await,
            AnyStepScheduler::Simulation(scheduler) => scheduler.wait_until(target).await,
            AnyStepScheduler::Clockless => std::future::pending().await,
        }
    }

    fn now(&self) -> Option<RobotInstant> {
        match self {
            AnyStepScheduler::Real(scheduler) => scheduler.now(),
            AnyStepScheduler::Simulation(scheduler) => scheduler.now(),
            AnyStepScheduler::Clockless => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use super::*;

    /// One fixed timeline for the scheduler tests, so `lt` reads like the
    /// nanosecond counter these tests actually care about.
    fn line() -> TimelineId {
        TimelineId::from_raw(1).expect("test timeline must be nonzero")
    }

    fn lt(ticks: u64) -> RobotInstant {
        RobotInstant::new(line(), ticks)
    }

    fn other(ticks: u64) -> RobotInstant {
        RobotInstant::new(
            TimelineId::from_raw(2).expect("test timeline must be nonzero"),
            ticks,
        )
    }

    /// The nominal step period for `RealScheduler` tests. They sleep on real
    /// time: Tokio's paused clock and the host boot clock are different clocks
    /// now, so `start_paused` would advance the timer while the instant the
    /// scheduler reports stayed put.
    const PERIOD: Duration = Duration::from_millis(10);

    /// The nominal step period for `SimulationScheduler` tests, expressed in
    /// the same tiny-tick units those tests use (they never touch the host
    /// clock, so the scale is arbitrary).
    const SIM_PERIOD: Duration = Duration::from_nanos(10);

    #[tokio::test]
    async fn real_scheduler_wakes_at_target_and_reports_no_miss_when_on_time() {
        let start = tokio::time::Instant::now();
        let scheduler =
            RealScheduler::new(MissedTick::Collapse, Some(PERIOD), lt(0)).expect("test host clock");

        let period_ns = PERIOD.as_nanos() as u64;
        let tick = scheduler.wait_until(lt(period_ns)).await;

        assert_eq!(tick.missed_ticks, 0);
        assert!(tick.fired_at.ticks() >= period_ns);
        assert!(start.elapsed() >= PERIOD);
    }

    #[test]
    fn real_cadence_collapses_a_missed_tick_instead_of_bursting() {
        let period_ns = PERIOD.as_nanos() as u64;
        // 50ms of boot clock against a 10ms period, one period already
        // consumed by the sleep to `target` itself: 4 further whole periods are
        // skipped, collapsed into this single released tick (no burst of 4
        // separate steps).
        let (fired, missed) = resolve_tick(
            0,
            Duration::from_millis(50),
            period_ns,
            Some(PERIOD),
            MissedTick::Collapse,
        );
        assert_eq!(fired, 50_000_000);
        assert_eq!(
            missed, 4,
            "a multi-period overrun collapses to one tick, reporting the skipped count"
        );
    }

    /// The suspend case, which is the whole reason cadence is resolved from the
    /// boot clock: Tokio's timer stops while the host sleeps, so it reports a
    /// tick that fired "on time" after an hour of nothing. The boot clock does
    /// not, and every skipped period is accounted for.
    #[test]
    fn a_host_suspend_counts_as_missed_periods_rather_than_time_that_never_happened() {
        let period_ns = PERIOD.as_nanos() as u64;
        let (fired, missed) = resolve_tick(
            0,
            Duration::from_secs(1),
            period_ns,
            Some(PERIOD),
            MissedTick::Collapse,
        );
        assert_eq!(
            fired, 1_000_000_000,
            "the released tick is where the host is"
        );
        assert_eq!(
            missed, 99,
            "one second of suspend is 99 further 10ms periods"
        );
    }

    #[test]
    fn a_tick_that_fires_on_time_reports_no_miss_and_no_period_means_no_collapse() {
        let period_ns = PERIOD.as_nanos() as u64;
        assert_eq!(
            resolve_tick(0, PERIOD, period_ns, Some(PERIOD), MissedTick::Collapse),
            (period_ns, 0)
        );
        assert_eq!(
            resolve_tick(
                0,
                Duration::from_millis(50),
                period_ns,
                None,
                MissedTick::Collapse
            ),
            (50_000_000, 0),
            "a step-less participant has no period to collapse against"
        );
        assert_eq!(
            resolve_tick(
                0,
                Duration::from_millis(50),
                period_ns,
                Some(PERIOD),
                MissedTick::CatchUp
            ),
            (50_000_000, 0),
            "catch-up replays each tick elsewhere; it never collapses here"
        );
    }

    #[tokio::test]
    async fn simulation_scheduler_releases_ticks_in_order_deterministically() {
        let (scheduler, handle) = SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD));

        let mut fired = Vec::new();
        for step in 1..=5u64 {
            let target = lt(step * 10);
            // Drive robot time forward from a concurrent task, exactly like
            // the live `simulation/clock` subscriber does.
            let handle = handle.clone();
            let advancer = tokio::spawn(async move { handle.advance(target) });
            let tick = scheduler.wait_until(target).await;
            advancer.await.unwrap();
            fired.push(tick.fired_at.ticks());
        }

        assert_eq!(fired, vec![10, 20, 30, 40, 50]);
    }

    #[test]
    fn a_scheduler_with_no_world_history_yet_reports_no_time_rather_than_zero() {
        let (scheduler, handle) = SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD));
        assert_eq!(
            scheduler.now(),
            None,
            "before the authority publishes, there is no world history at all"
        );
        handle.advance(lt(10));
        assert_eq!(scheduler.now(), Some(lt(10)));
    }

    #[test]
    fn timeline_replacement_is_equality_only_with_no_generation_order() {
        let (scheduler, handle) = SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD));

        assert_eq!(handle.advance(lt(100)), SimulationClockAdvance::Advanced);
        // A different timeline replaces the active one regardless of any
        // numeric relationship: identities are opaque.
        assert_eq!(handle.advance(other(0)), SimulationClockAdvance::Advanced);
        assert_eq!(scheduler.now(), Some(other(0)));

        assert_eq!(
            handle.advance(lt(101)),
            SimulationClockAdvance::RetiredTimeline,
            "a late clock from the retired world must not reactivate it"
        );
        assert_eq!(
            handle.advance(other(0)),
            SimulationClockAdvance::DuplicateOrBackward
        );
        assert_eq!(
            scheduler.now(),
            Some(other(0)),
            "duplicate and same-timeline non-forward samples are ignored"
        );
    }

    #[tokio::test]
    async fn simulation_scheduler_never_sleeps_on_a_wall_clock_timer() {
        // No `start_paused`/`tokio::time::advance` needed: the scheduler must
        // resolve purely from the world-authority feed, with no real waiting.
        let (scheduler, handle) = SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD));
        handle.advance(lt(100));

        let started = std::time::Instant::now();
        let tick = scheduler.wait_until(lt(100)).await;
        let elapsed = started.elapsed();

        assert_eq!(tick.fired_at, lt(100));
        assert!(
            elapsed < Duration::from_millis(50),
            "simulation scheduler should resolve immediately once time is already due, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn simulation_scheduler_releases_when_advance_happened_before_wait() {
        let (scheduler, handle) = SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD));

        handle.advance(lt(30));
        let tick = tokio::time::timeout(Duration::from_millis(50), scheduler.wait_until(lt(20)))
            .await
            .expect("advance-before-wait should release without waiting for another change");

        assert_eq!(tick.fired_at, lt(30));
        assert_eq!(tick.missed_ticks, 1);
    }

    #[tokio::test]
    async fn simulation_scheduler_does_not_miss_racing_advance_after_pending_poll() {
        let (scheduler, handle) = SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD));

        let wait = scheduler.wait_until(lt(10));
        tokio::pin!(wait);
        assert!(
            poll_once(wait.as_mut()).is_none(),
            "wait should pend before robot time reaches the target"
        );

        handle.advance(lt(10));
        let tick = tokio::time::timeout(Duration::from_secs(1), &mut wait)
            .await
            .expect("advance after a pending poll should wake the waiter");
        assert_eq!(tick.fired_at, lt(10));
        assert_eq!(tick.missed_ticks, 0);
    }

    #[tokio::test]
    async fn simulation_scheduler_keeps_waiting_if_external_handle_is_dropped() {
        let (scheduler, handle) = SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD));
        drop(handle);

        let wait = scheduler.wait_until(lt(10));
        tokio::pin!(wait);

        assert!(
            poll_once(wait.as_mut()).is_none(),
            "dropping the external handle must not close the scheduler feed and release a stale tick"
        );
    }

    #[tokio::test]
    async fn simulation_scheduler_collapses_a_multi_period_jump() {
        let (scheduler, handle) = SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD));

        // Jump straight past three periods (10 ticks each at this test scale)
        // before the scheduler ever waits.
        handle.advance(lt(40));
        let tick = scheduler.wait_until(lt(10)).await;

        assert_eq!(
            tick.missed_ticks, 3,
            "a jump past the target collapses to one released tick, reporting all 3 skipped periods"
        );
        assert_eq!(tick.fired_at, lt(40));
    }

    /// Poll `fut` exactly once with a no-op waker, returning `Some(output)` if
    /// it was immediately ready or `None` if it is still pending. Used to
    /// assert "not ready yet" deterministically, without racing a real sleep.
    fn poll_once<F: Future>(fut: std::pin::Pin<&mut F>) -> Option<F::Output> {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn noop(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        // SAFETY: the vtable's clone/wake/drop are all no-ops over a null data
        // pointer, so every operation is trivially valid.
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        match fut.poll(&mut cx) {
            Poll::Ready(output) => Some(output),
            Poll::Pending => None,
        }
    }
}
