//! Wall-clock step scheduling.

use std::time::Duration;

use super::{SchedulerTick, StepScheduler};
use crate::bus::{LocalInstant, RobotInstant, TimelineId};
use crate::participant::duration_nanos;

/// Wall-clock scheduler: wraps [`tokio::time::Instant`] sleeps. This is the
/// default, and real cadence never waits on a bus message.
///
/// A [`RobotInstant`] carries no wall-clock unit of its own, so
/// [`RealScheduler`] sleeps on Tokio's timer but never *reads* time from it:
/// the timer decides when to wake, and the host's suspend-aware boot clock
/// ([`LocalInstant`]) decides what instant was actually reached. A host that
/// suspends for an hour therefore resumes with an hour of missed periods, not
/// with an hour that never happened.
pub(crate) struct RealScheduler {
    /// The nominal step period, needed to collapse a multi-period overrun
    /// into a single released tick (see [`Self::resolve_tick`]). `None` when the
    /// participant has no `Participant::step` schedule at all.
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
    /// reports at start), running `period`. `period` is `None` for a step-less
    /// participant, in which case [`Self::wait_until`] is never called by the
    /// runner.
    ///
    /// `None` when the host boot clock cannot be read: without an anchor there
    /// is no cadence to run, which the caller reports as ordinary failure.
    pub(crate) fn new(period: Option<Duration>, now: RobotInstant) -> Option<Self> {
        // The boot anchor is sampled *first*, so the timer anchor is never
        // earlier than it. Sampling the other way round leaves the timer able
        // to reach its deadline while the boot clock has covered slightly less
        // ground, which would release a tick that reads as earlier than the
        // target it was released for.
        let started_boot = LocalInstant::try_now()?;
        Some(RealScheduler {
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

    /// Resolve a released tick from `elapsed` boot-clock time, independent of
    /// the timer that woke the task: the released ticks, and how many whole
    /// periods were skipped to reach them.
    ///
    /// Taking `elapsed` as an argument rather than reading the clock is what
    /// makes the only arithmetic that matters testable without a host suspend.
    ///
    /// There is no early-wake case to defend against. If the timer's clock stops
    /// during suspend while the boot clock keeps counting, the wake is *late* in
    /// boot terms; if both count, they move together; and while the host is
    /// awake a monotonic timer cannot fire before its own deadline. So one
    /// boot-clock read after the wake is enough - no re-sleep loop.
    fn resolve_tick(&self, elapsed: Duration, target_ticks: u64) -> (u64, u32) {
        // A tick is never released before the instant it was released *for*:
        // the boot anchor precedes the timer anchor, so this clamp only ever
        // absorbs that construction skew.
        let fired_ticks = self
            .started_ticks
            .saturating_add(duration_nanos(elapsed))
            .max(target_ticks);
        // After an overrun, fire once and record how many periods were skipped
        // rather than replaying each missed tick back-to-back.
        //
        // The count is arithmetic rather than a period-by-period walk: an
        // overrun is unbounded in principle (a suspended host), and a loop over
        // it is a hang rather than a slow answer.
        let mut missed_ticks = 0u32;
        if let Some(period) = self.period.filter(|period| !period.is_zero()) {
            let period_ns = duration_nanos(period);
            let overrun = fired_ticks.saturating_sub(target_ticks);
            missed_ticks = u32::try_from(overrun / period_ns).unwrap_or(u32::MAX);
        }
        (fired_ticks, missed_ticks)
    }
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
        let (fired_ticks, missed_ticks) = self.resolve_tick(elapsed, target.ticks());
        SchedulerTick {
            fired_at: RobotInstant::new(self.timeline, fired_ticks),
            missed_ticks,
        }
    }

    fn now(&self) -> Option<RobotInstant> {
        Some(RobotInstant::new(
            self.timeline,
            self.started_ticks
                .saturating_add(duration_nanos(self.boot_elapsed()?)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One fixed timeline for the scheduler tests, so `lt` reads like the
    /// nanosecond counter these tests actually care about.
    fn lt(ticks: u64) -> RobotInstant {
        RobotInstant::new(
            TimelineId::from_raw(1).expect("test timeline must be nonzero"),
            ticks,
        )
    }

    /// The nominal step period for these tests. They sleep on real time: Tokio's
    /// paused clock and the host boot clock are different clocks, so
    /// `start_paused` would advance the timer while the instant the scheduler
    /// reports stayed put.
    const PERIOD: Duration = Duration::from_millis(10);

    /// A scheduler anchored at tick zero, so a resolved tick reads directly as
    /// the nanosecond count the arithmetic produced.
    fn anchored(period: Option<Duration>) -> RealScheduler {
        RealScheduler::new(period, lt(0)).expect("test host clock")
    }

    #[tokio::test]
    async fn real_scheduler_wakes_at_target_and_reports_no_miss_when_on_time() {
        let start = tokio::time::Instant::now();
        let scheduler = anchored(Some(PERIOD));

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
        let (fired, missed) =
            anchored(Some(PERIOD)).resolve_tick(Duration::from_millis(50), period_ns);
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
        let (fired, missed) =
            anchored(Some(PERIOD)).resolve_tick(Duration::from_secs(1), period_ns);
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
            anchored(Some(PERIOD)).resolve_tick(PERIOD, period_ns),
            (period_ns, 0)
        );
        assert_eq!(
            anchored(None).resolve_tick(Duration::from_millis(50), period_ns),
            (50_000_000, 0),
            "a step-less participant has no period to collapse against"
        );
    }
}
