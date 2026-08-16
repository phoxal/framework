//! The real-execution clock.

use std::sync::Mutex;

use super::{ClockReading, ClockSource, TimeUnsynchronized};
use crate::bus::{LocalInstant, RobotInstant, TimelineId};
use crate::participant::lock;

/// The real-execution clock: the host boot clock, read straight onto the
/// execution's timeline.
///
/// Robot time zero is host boot, so there is no origin to subtract and nothing
/// for a launcher to hand over: a
/// tick *is* a nanosecond of `CLOCK_BOOTTIME`. The domain is host-wide, so two
/// processes on one host compute the same [`RobotInstant`] for the same physical
/// moment without exchanging a message, and they do so without having to agree
/// on an anchor first. Suspend counts, because [`LocalInstant`] reads the
/// continuous clock.
#[derive(Debug)]
pub(crate) struct RealClock {
    timeline: TimelineId,
    last_ticks: Mutex<u64>,
}

impl RealClock {
    /// A clock producing instants on `timeline`, the real timeline of one
    /// execution.
    pub(crate) const fn new(timeline: TimelineId) -> Self {
        RealClock {
            timeline,
            last_ticks: Mutex::new(0),
        }
    }
}

impl ClockSource for RealClock {
    fn read(&self) -> ClockReading {
        if LocalInstant::clock_faulted() {
            return ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault);
        }
        let Some(now) = LocalInstant::try_now() else {
            return ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault);
        };
        let ticks = now.boot_ns();
        // The boot clock is monotonic, so this is a defensive latch rather than
        // a correction: a regression means the clock read is untrustworthy.
        let mut last = lock(&self.last_ticks);
        if ticks < *last {
            return ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault);
        }
        *last = ticks;
        ClockReading::Synchronized(RobotInstant::new(self.timeline, ticks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn timeline() -> TimelineId {
        TimelineId::from_raw(0x0123_4567_89ab_cdef).expect("a nonzero test timeline")
    }

    #[test]
    fn the_real_clock_shares_one_host_wide_domain_across_processes() {
        // Two independently constructed clocks on one execution timeline model
        // two processes on one host: because both read the host boot clock and
        // both project it onto the timeline the execution id fixes, they
        // compute directly comparable instants with nothing exchanged between
        // them - the property the cross-process freshness checks rely on.
        let a = RealClock::new(timeline());
        let b = RealClock::new(timeline());
        let ta = a.read().instant().expect("clock a must be synchronized");
        let tb = b.read().instant().expect("clock b must be synchronized");
        assert_eq!(ta.timeline(), tb.timeline());
        let gap = tb
            .duration_since(ta)
            .expect("same timeline must be comparable");
        assert!(
            gap < Duration::from_secs(1),
            "two host clocks disagree by {gap:?}"
        );
    }

    /// Robot time zero is host boot, so a real reading is the boot-clock
    /// reading itself rather than an offset from an anchor somebody minted.
    #[test]
    fn a_real_tick_is_a_nanosecond_of_the_host_boot_clock() {
        let before = LocalInstant::try_now().expect("test host clock");
        let now = RealClock::new(timeline())
            .read()
            .instant()
            .expect("the clock must be synchronized");
        let after = LocalInstant::try_now().expect("test host clock");
        assert!(
            before.boot_ns() <= now.ticks() && now.ticks() <= after.boot_ns(),
            "robot tick {} escaped the boot-clock interval {}..={}",
            now.ticks(),
            before.boot_ns(),
            after.boot_ns()
        );
    }

    #[test]
    fn the_real_clock_never_regresses_within_a_timeline() {
        let clock = RealClock::new(timeline());
        let mut last = clock.read().instant().expect("test clock reads");
        for _ in 0..1000 {
            let next = clock.read().instant().expect("test clock reads");
            assert!(
                next.checked_cmp(last).expect("one timeline") != std::cmp::Ordering::Less,
                "robot time regressed: {next} < {last}"
            );
            last = next;
        }
    }
}
