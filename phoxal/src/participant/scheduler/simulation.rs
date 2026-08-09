//! Logical-time step scheduling, driven by the world authority.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use super::{SchedulerTick, StepScheduler};
use crate::bus::RobotInstant;
use crate::participant::clock::simulation::SimulationClock;
use crate::participant::{duration_nanos, lock};
use phoxal_bus::RetiredTimelines;

/// Simulation scheduler: releases ticks from **robot** time advanced by the
/// world authority, never a real sleep.
///
/// # The live seam
///
/// The simulation controller is the authoritative owner of the
/// `runtime/simulation/clock` hand. In simulation mode the participant runner
/// subscribes that topic and forwards each observed [`RobotInstant`] into this
/// scheduler through [`SimulationClockHandle::advance`]. Tests drive the same
/// handle directly, so live and deterministic test paths share the scheduler
/// boundary.
///
/// # Determinism
///
/// [`SimulationScheduler::wait_until`] never sleeps on a wall-clock timer: it
/// awaits a [`tokio::sync::watch`] change, so a test drives robot time
/// forward with [`SimulationClockHandle::advance`] and gets deterministic tick
/// order with no real waiting. Clock silence is the only pause signal.
pub(crate) struct SimulationScheduler {
    /// The nominal step period, used to count how many whole periods a
    /// logical-time jump spanned (see [`Self::wait_until`]). `None` when the
    /// participant has no `Participant::step` schedule.
    period: Option<Duration>,
    /// Keeps the watch channel open even when the runner has not wired an
    /// external `runtime/simulation/clock` feed yet. Without this, dropping the
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SimulationClockAdvance {
    Advanced,
    DuplicateOrBackward,
    RetiredTimeline,
}

/// A cloneable handle that advances the logical time a
/// [`SimulationScheduler`] observes.
///
/// This is the seam a live `runtime/simulation/clock` bus subscription attaches to
/// (see [`SimulationScheduler`] docs): a subscriber task calls
/// [`advance`](Self::advance) once per received sample. Tests use the same
/// method to drive the scheduler deterministically, with no bus and no real
/// sleeping.
#[derive(Clone)]
pub(crate) struct SimulationClockHandle {
    tx: watch::Sender<Option<RobotInstant>>,
    state: Arc<Mutex<SimulationClockState>>,
}

impl SimulationClockHandle {
    /// Advance the observed robot time to `at`. A no-op if `at` is a duplicate
    /// or backwards within the active timeline. Any different timeline replaces
    /// the active world history, since timelines are opaque identities with no
    /// generation order; recently retired timelines are ignored so an in-flight
    /// clock from a dead controller cannot reactivate old state.
    pub(crate) fn advance(&self, at: RobotInstant) -> SimulationClockAdvance {
        let mut state = lock(&self.state);
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
    /// Build a simulation scheduler running `period`, plus its driving handle.
    /// A logical-time jump spanning multiple periods fires once and reports the
    /// skipped count, mirroring [`RealScheduler`](super::real::RealScheduler)'s
    /// behavior for a wall-clock overrun. `period` is `None` for a step-less
    /// participant, in which case [`Self::wait_until`] is never called by the
    /// runner.
    pub(crate) fn new(period: Option<Duration>) -> (Self, SimulationClockHandle) {
        // No seed: there is no world history until the authority publishes one,
        // and an invented instant zero of an invented timeline would be a world
        // nobody authored.
        let (tx, rx) = watch::channel(None);
        let scheduler = SimulationScheduler {
            period,
            _tx_keepalive: tx.clone(),
            rx,
        };
        let handle = SimulationClockHandle {
            tx,
            state: Arc::new(Mutex::new(SimulationClockState {
                current: None,
                retired_timelines: RetiredTimelines::default(),
            })),
        };
        (scheduler, handle)
    }

    /// A [`SimulationClock`] that observes the same feed this scheduler releases
    /// ticks from. The runner uses it as the instant source in simulation mode
    /// so `Participant::step` release time and stamped production time never
    /// diverge.
    pub(crate) fn simulation_clock(&self) -> SimulationClock {
        SimulationClock::from_receiver(self.rx.clone())
    }

    /// A receiver on the same logical-time feed, for observing timeline
    /// replacement independently of the step cadence.
    pub(crate) fn time_receiver(&self) -> watch::Receiver<Option<RobotInstant>> {
        self.rx.clone()
    }

    fn missed_ticks(&self, target: RobotInstant, current: RobotInstant) -> u32 {
        let Some(period) = self.period else {
            return 0;
        };
        let Ok(overrun) = current.duration_since(target) else {
            return 0;
        };
        let period_ns = duration_nanos(period);
        if period_ns == 0 {
            return 0;
        }
        u32::try_from(duration_nanos(overrun) / period_ns).unwrap_or(u32::MAX)
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
                    // When the feed had already advanced past `target` (by one
                    // or more whole periods) before we even started waiting,
                    // fire once and report how many periods were skipped rather
                    // than have the runner replay each one - the same "no
                    // catch-up storm" behavior as `RealScheduler`'s overrun
                    // handling.
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

#[cfg(test)]
mod tests {
    use std::future::Future;

    use super::*;
    use crate::bus::TimelineId;

    /// One fixed timeline, so `lt` reads like the tick counter these tests
    /// actually care about.
    fn lt(ticks: u64) -> RobotInstant {
        RobotInstant::new(
            TimelineId::from_raw(1).expect("test timeline must be nonzero"),
            ticks,
        )
    }

    fn other(ticks: u64) -> RobotInstant {
        RobotInstant::new(
            TimelineId::from_raw(2).expect("test timeline must be nonzero"),
            ticks,
        )
    }

    /// The nominal step period, expressed in the same tiny-tick units these
    /// tests use (they never touch the host clock, so the scale is arbitrary).
    const SIM_PERIOD: Duration = Duration::from_nanos(10);

    #[tokio::test]
    async fn simulation_scheduler_releases_ticks_in_order_deterministically() {
        let (scheduler, handle) = SimulationScheduler::new(Some(SIM_PERIOD));

        let mut fired = Vec::new();
        for step in 1..=5u64 {
            let target = lt(step * 10);
            // Drive robot time forward from a concurrent task, exactly like
            // the live `runtime/simulation/clock` subscriber does.
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
        let (scheduler, handle) = SimulationScheduler::new(Some(SIM_PERIOD));
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
        let (scheduler, handle) = SimulationScheduler::new(Some(SIM_PERIOD));

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
        let (scheduler, handle) = SimulationScheduler::new(Some(SIM_PERIOD));
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
        let (scheduler, handle) = SimulationScheduler::new(Some(SIM_PERIOD));

        handle.advance(lt(30));
        let tick = tokio::time::timeout(Duration::from_millis(50), scheduler.wait_until(lt(20)))
            .await
            .expect("advance-before-wait should release without waiting for another change");

        assert_eq!(tick.fired_at, lt(30));
        assert_eq!(tick.missed_ticks, 1);
    }

    #[tokio::test]
    async fn simulation_scheduler_does_not_miss_racing_advance_after_pending_poll() {
        let (scheduler, handle) = SimulationScheduler::new(Some(SIM_PERIOD));

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
        let (scheduler, handle) = SimulationScheduler::new(Some(SIM_PERIOD));
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
        let (scheduler, handle) = SimulationScheduler::new(Some(SIM_PERIOD));

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
