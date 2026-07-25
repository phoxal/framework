//! The step scheduler (D34/#09): split "time reading" from "tick release".
//!
//! [`ClockSource`](crate::participant::clock::ClockSource) answers "what time is
//! it", and every produced timestamp is stamped from it. [`StepScheduler`]
//! answers a different question: "when should the next `#[step]` tick fire".
//! Real mode answers that from wall time (unchanged behavior); a true
//! simulation clock instead releases ticks only when the authoritative
//! `simulation/clock` (owned by the `Simulator` kind, decisions.md D1) advances
//! logical time. Without this split, simulated time could label samples but
//! could never drive the loop - the runner would still free-run on the wall
//! clock underneath a "simulated" label.
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

use crate::bus::LogicalTime;
use crate::participant::spec::MissedTick;
use phoxal_bus::RetiredEpochs;

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
    /// The logical time the tick fired at (`>= target`, modulo the scheduler's
    /// own clamping/collapse policy).
    pub fired_at: LogicalTime,
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
    async fn wait_until(&self, target: LogicalTime) -> SchedulerTick;

    /// The scheduler's own view of "now", in the same logical-time domain as
    /// [`SchedulerTick::fired_at`]. For [`RealScheduler`] this agrees with the
    /// wall clock; for [`SimulationScheduler`] it is the last logical time
    /// observed from the feed.
    fn now(&self) -> LogicalTime;
}

/// Wall-clock scheduler: wraps [`tokio::time::Instant`] sleeps. This is the
/// default and must reproduce the runner's pre-#09 cadence/collapse behavior
/// exactly - real mode is not allowed to regress.
///
/// `LogicalTime` has no wall-clock unit of its own (D34: it is an
/// epoch + a nanosecond counter in whatever domain the active `ClockSource`
/// defines), so [`RealScheduler`] tracks the wall-time deadline internally as
/// a [`tokio::time::Instant`] and only translates to/from `LogicalTime` at the
/// edges (`wait_until`'s `target`/`fired_at`, `now()`). The nanosecond value
/// carried by `LogicalTime` is assumed to be wall-clock nanoseconds in real
/// mode (true for [`RealClock`](crate::participant::clock::RealClock), the
/// only `ClockSource` real mode ships with).
pub struct RealScheduler {
    missed_tick: MissedTick,
    /// The nominal step period, needed to collapse a multi-period overrun
    /// into a single released tick (see [`Self::wait_until`]). `None` when the
    /// participant has no `#[step]` schedule at all.
    period: Option<Duration>,
    epoch: u64,
    started_at: tokio::time::Instant,
    started_ns: u64,
}

impl RealScheduler {
    /// A real scheduler anchored to `now` (the epoch/time the runner's clock
    /// reports at start), running `period` and applying `missed_tick` after an
    /// overrun. `period` is `None` for a step-less participant, in which case
    /// [`Self::wait_until`] is never called by the runner.
    pub fn new(missed_tick: MissedTick, period: Option<Duration>, now: LogicalTime) -> Self {
        RealScheduler {
            missed_tick,
            period,
            epoch: now.epoch(),
            started_at: tokio::time::Instant::now(),
            started_ns: now.time_ns(),
        }
    }

    /// Convert a `LogicalTime` in this scheduler's epoch to the equivalent
    /// wall-clock `Instant`, anchored at construction.
    fn instant_for(&self, target: LogicalTime) -> tokio::time::Instant {
        let delta_ns = target.time_ns().saturating_sub(self.started_ns);
        self.started_at + Duration::from_nanos(delta_ns)
    }

    /// Convert a wall-clock `Instant` back to this scheduler's `LogicalTime`
    /// domain.
    fn logical_for(&self, instant: tokio::time::Instant) -> LogicalTime {
        let elapsed = instant.saturating_duration_since(self.started_at);
        LogicalTime::new(
            self.epoch,
            self.started_ns
                .saturating_add(duration_to_nanos_saturating(elapsed)),
        )
    }
}

impl StepScheduler for RealScheduler {
    async fn wait_until(&self, target: LogicalTime) -> SchedulerTick {
        let deadline = self.instant_for(target);
        tokio::time::sleep_until(deadline).await;

        // Collapse policy (D34, the v1 default): after an overrun, fire once
        // and record how many periods were skipped rather than replaying each
        // missed tick back-to-back (no catch-up storm). `CatchUp` is reserved
        // for offline replay and is not driven by this scheduler (#09 scope:
        // real + simulation only) - it resolves at `target` with no
        // collapsing, i.e. `missed_ticks` is always 0.
        let mut missed_ticks = 0u32;
        if self.missed_tick == MissedTick::Collapse {
            if let Some(period) = self.period.filter(|period| !period.is_zero()) {
                let real_now = tokio::time::Instant::now();
                let mut next_deadline = deadline;
                while next_deadline + period <= real_now {
                    next_deadline += period;
                    missed_ticks = missed_ticks.saturating_add(1);
                }
            }
        }

        SchedulerTick {
            fired_at: self.logical_for(tokio::time::Instant::now()),
            missed_ticks,
        }
    }

    fn now(&self) -> LogicalTime {
        self.logical_for(tokio::time::Instant::now())
    }
}

/// Simulation scheduler: releases ticks from **logical** time advanced by an
/// external source, never a real sleep.
///
/// # The live seam
///
/// The Webots controller is the authoritative owner of the
/// `simulation/clock` state topic.
/// In simulation mode the participant runner subscribes that topic through
/// `spawn_simulation_clock_feed` and forwards each observed `LogicalTime` into
/// this scheduler through [`SimulationClockHandle::advance`].
/// Tests drive the same handle directly, so live and deterministic test paths
/// share the scheduler boundary.
///
/// # Determinism
///
/// [`SimulationScheduler::wait_until`] never sleeps on a wall-clock timer: it
/// awaits a [`tokio::sync::watch`] change, so a test drives logical time
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
    _tx_keepalive: watch::Sender<LogicalTime>,
    rx: watch::Receiver<LogicalTime>,
}

struct SimulationClockState {
    current: LogicalTime,
    retired_epochs: RetiredEpochs,
}

/// Result of applying one clock sample to a simulation scheduler.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimulationClockAdvance {
    Advanced,
    DuplicateOrBackward,
    RetiredEpoch,
    ReservedEpoch,
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
    tx: watch::Sender<LogicalTime>,
    state: std::sync::Arc<std::sync::Mutex<SimulationClockState>>,
}

impl SimulationClockHandle {
    /// Advance the observed logical time to `time`. A no-op (time does not
    /// move forwards within the active epoch) if `time` is duplicate or
    /// backwards. Any new opaque epoch replaces the active execution,
    /// regardless of numeric value; recently retired epochs are ignored so an
    /// in-flight clock from a dead controller cannot reactivate old state.
    pub fn advance(&self, time: LogicalTime) -> SimulationClockAdvance {
        if time.epoch() == 0 {
            return SimulationClockAdvance::ReservedEpoch;
        }
        let mut state = self.state.lock().expect("simulation clock mutex poisoned");
        if time.epoch() == state.current.epoch() {
            if time.time_ns() <= state.current.time_ns() {
                return SimulationClockAdvance::DuplicateOrBackward;
            }
        } else {
            if state.retired_epochs.contains(time.epoch()) {
                return SimulationClockAdvance::RetiredEpoch;
            }
            let previous_epoch = state.current.epoch();
            state.retired_epochs.retire(previous_epoch);
            state.retired_epochs.activate(time.epoch());
        }
        state.current = time;
        self.tx.send_replace(time);
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
    pub fn new(
        missed_tick: MissedTick,
        period: Option<Duration>,
        start: LogicalTime,
    ) -> (Self, SimulationClockHandle) {
        let (tx, rx) = watch::channel(start);
        let scheduler = SimulationScheduler {
            missed_tick,
            period,
            _tx_keepalive: tx.clone(),
            rx,
        };
        let handle = SimulationClockHandle {
            tx,
            state: std::sync::Arc::new(std::sync::Mutex::new(SimulationClockState {
                current: start,
                retired_epochs: RetiredEpochs::default(),
            })),
        };
        (scheduler, handle)
    }

    /// A [`SimulationClock`](crate::participant::clock::SimulationClock) that
    /// observes the same logical-time feed this scheduler releases ticks from.
    /// The runner uses it as the timestamp source in simulation mode so
    /// `#[step]` release time and stamped `produced_at_ns` never diverge.
    pub(crate) fn simulation_clock(&self) -> crate::participant::clock::SimulationClock {
        crate::participant::clock::SimulationClock::from_receiver(self.rx.clone())
    }

    fn missed_ticks(&self, target: LogicalTime, current: LogicalTime) -> u32 {
        let (MissedTick::Collapse, Some(period)) = (self.missed_tick, self.period) else {
            return 0;
        };
        if current.epoch() != target.epoch() || current.time_ns() <= target.time_ns() {
            return 0;
        }

        let period_ns = duration_to_nanos_saturating(period);
        if period_ns == 0 {
            return 0;
        }

        let overrun_ns = current.time_ns().saturating_sub(target.time_ns());
        u32::try_from(overrun_ns / period_ns).unwrap_or(u32::MAX)
    }
}

impl StepScheduler for SimulationScheduler {
    async fn wait_until(&self, target: LogicalTime) -> SchedulerTick {
        let mut rx = self.rx.clone();
        loop {
            let current = *rx.borrow_and_update();
            if current.epoch() != target.epoch() || current.time_ns() >= target.time_ns() {
                // Collapse policy (D34): when the feed had already advanced
                // past `target` (by one or more whole periods) before we even
                // started waiting, fire once and report how many periods were
                // skipped, rather than the runner replaying each one - the
                // same "no catch-up storm" behavior as `RealScheduler`'s
                // wall-clock overrun.
                let missed_ticks = self.missed_ticks(target, current);
                return SchedulerTick {
                    fired_at: current,
                    missed_ticks,
                };
            }
            if rx.changed().await.is_err() {
                // `SimulationScheduler` owns a sender keepalive, so this is
                // only defensive for future constructors: report the last
                // known time rather than hang on a closed feed.
                return SchedulerTick {
                    fired_at: *rx.borrow_and_update(),
                    missed_ticks: 0,
                };
            }
        }
    }

    fn now(&self) -> LogicalTime {
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
}

impl AnyStepScheduler {
    /// Subscribe to logical-time changes when this is a simulation scheduler.
    ///
    /// The runner uses this independently of the optional step cadence so
    /// epoch replacement is still observed by clocked, step-less services.
    pub(crate) fn simulation_time_receiver(&self) -> Option<watch::Receiver<LogicalTime>> {
        match self {
            AnyStepScheduler::Real(_) => None,
            AnyStepScheduler::Simulation(scheduler) => Some(scheduler.rx.clone()),
        }
    }
}

impl StepScheduler for AnyStepScheduler {
    async fn wait_until(&self, target: LogicalTime) -> SchedulerTick {
        match self {
            AnyStepScheduler::Real(scheduler) => scheduler.wait_until(target).await,
            AnyStepScheduler::Simulation(scheduler) => scheduler.wait_until(target).await,
        }
    }

    fn now(&self) -> LogicalTime {
        match self {
            AnyStepScheduler::Real(scheduler) => scheduler.now(),
            AnyStepScheduler::Simulation(scheduler) => scheduler.now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lt(ns: u64) -> LogicalTime {
        LogicalTime::new(1, ns)
    }

    /// The nominal step period for `RealScheduler` tests, which sleep on real
    /// (paused/advanced) Tokio time.
    const PERIOD: Duration = Duration::from_millis(10);

    /// The nominal step period for `SimulationScheduler` tests, expressed in
    /// the same tiny-nanosecond `LogicalTime` units those tests use (they
    /// never touch the wall clock, so the unit is arbitrary).
    const SIM_PERIOD: Duration = Duration::from_nanos(10);

    #[tokio::test(start_paused = true)]
    async fn real_scheduler_wakes_at_target_and_reports_no_miss_when_on_time() {
        let start = tokio::time::Instant::now();
        let scheduler = RealScheduler::new(MissedTick::Collapse, Some(PERIOD), lt(0));

        let period_ns = PERIOD.as_nanos() as u64;
        let tick = scheduler.wait_until(lt(period_ns)).await;

        assert_eq!(tick.missed_ticks, 0);
        assert!(tick.fired_at.time_ns() >= period_ns);
        assert!(start.elapsed() >= PERIOD);
    }

    #[tokio::test(start_paused = true)]
    async fn real_scheduler_collapses_a_missed_tick_instead_of_bursting() {
        let scheduler = RealScheduler::new(MissedTick::Collapse, Some(PERIOD), lt(0));

        // Advance real time far past the target before ever polling
        // `wait_until`, simulating an overrun step.
        tokio::time::advance(Duration::from_millis(50)).await;

        let period_ns = PERIOD.as_nanos() as u64;
        let tick = scheduler.wait_until(lt(period_ns)).await;

        // 50ms elapsed against a 10ms period, one period already consumed by
        // the `sleep_until(target)` itself: 4 further whole periods are
        // skipped, collapsed into this single released tick (no burst of 4
        // separate steps).
        assert_eq!(
            tick.missed_ticks, 4,
            "a multi-period overrun collapses to one tick, reporting the skipped count"
        );
    }

    #[tokio::test]
    async fn simulation_scheduler_releases_ticks_in_order_deterministically() {
        let (scheduler, handle) =
            SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD), lt(0));

        let mut fired = Vec::new();
        for step in 1..=5u64 {
            let target = lt(step * 10);
            // Drive logical time forward from a concurrent task, exactly like
            // a future `simulation/clock` bus subscriber would.
            let handle = handle.clone();
            let advance_target = target;
            let advancer = tokio::spawn(async move { handle.advance(advance_target) });
            let tick = scheduler.wait_until(target).await;
            advancer.await.unwrap();
            fired.push(tick.fired_at.time_ns());
        }

        assert_eq!(fired, vec![10, 20, 30, 40, 50]);
    }

    #[test]
    fn simulation_epoch_replacement_is_equality_only_in_both_numeric_directions() {
        let (scheduler, handle) = SimulationScheduler::new(
            MissedTick::Collapse,
            Some(SIM_PERIOD),
            LogicalTime::new(5, 100),
        );

        assert_eq!(
            handle.advance(LogicalTime::new(2, 0)),
            SimulationClockAdvance::Advanced
        );
        assert_eq!(scheduler.now(), LogicalTime::new(2, 0));
        assert_eq!(
            handle.advance(LogicalTime::new(9, 0)),
            SimulationClockAdvance::Advanced
        );
        assert_eq!(scheduler.now(), LogicalTime::new(9, 0));

        assert_eq!(
            handle.advance(LogicalTime::new(2, 1)),
            SimulationClockAdvance::RetiredEpoch,
            "a late clock from the retired execution must not reactivate it"
        );
        assert_eq!(
            handle.advance(LogicalTime::new(9, 0)),
            SimulationClockAdvance::DuplicateOrBackward
        );
        assert_eq!(
            handle.advance(LogicalTime::new(0, 1)),
            SimulationClockAdvance::ReservedEpoch
        );
        assert_eq!(
            scheduler.now(),
            LogicalTime::new(9, 0),
            "duplicate and same-epoch non-forward samples are ignored"
        );
    }

    #[tokio::test]
    async fn simulation_scheduler_never_sleeps_on_a_wall_clock_timer() {
        // No `start_paused`/`tokio::time::advance` needed: the scheduler must
        // resolve purely from the logical-time feed, with no real waiting.
        let (scheduler, handle) =
            SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD), lt(0));
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
        let (scheduler, handle) =
            SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD), lt(0));

        handle.advance(lt(30));
        let tick = tokio::time::timeout(Duration::from_millis(50), scheduler.wait_until(lt(20)))
            .await
            .expect("advance-before-wait should release without waiting for another change");

        assert_eq!(tick.fired_at, lt(30));
        assert_eq!(tick.missed_ticks, 1);
    }

    #[tokio::test]
    async fn simulation_scheduler_does_not_miss_racing_advance_after_pending_poll() {
        let (scheduler, handle) =
            SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD), lt(0));

        let wait = scheduler.wait_until(lt(10));
        tokio::pin!(wait);
        assert!(
            poll_once(wait.as_mut()).is_none(),
            "wait should pend before logical time reaches the target"
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
        let (scheduler, handle) =
            SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD), lt(0));
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
        let (scheduler, handle) =
            SimulationScheduler::new(MissedTick::Collapse, Some(SIM_PERIOD), lt(0));

        // Jump straight past three periods (10ns each in this LogicalTime unit
        // test scale) before the scheduler ever waits.
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
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        match fut.poll(&mut cx) {
            Poll::Ready(output) => Some(output),
            Poll::Pending => None,
        }
    }
}
