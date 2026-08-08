//! The real-execution clock.

use std::sync::Mutex;

use super::{ClockReading, ClockSource, TimeUnsynchronized};
use crate::bus::{LocalInstant, RobotInstant};
use crate::participant::lock;
use phoxal_runtime_contract::origin::{BootId, ExecutionOrigin};

/// The real-execution clock: the host boot clock, offset by the execution
/// origin, projected onto the execution's timeline.
///
/// The domain is host-wide, so two processes on one host compute the same
/// [`RobotInstant`] for the same physical moment without exchanging a message.
/// Suspend counts, because [`LocalInstant`] reads the continuous clock.
#[derive(Debug)]
pub(crate) struct RealClock {
    origin: ExecutionOrigin,
    last_ticks: Mutex<u64>,
}

impl RealClock {
    /// A clock anchored at `origin`, validated against this host's boot.
    pub(crate) fn new(origin: ExecutionOrigin) -> Result<Self, TimeUnsynchronized> {
        if origin.boot() != BootId::current() {
            return Err(TimeUnsynchronized::ForeignBoot);
        }
        Ok(RealClock {
            origin,
            last_ticks: Mutex::new(0),
        })
    }
}

impl ClockSource for RealClock {
    fn read(&self) -> ClockReading {
        if LocalInstant::clock_faulted() {
            return ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault);
        }
        let origin = self.origin;
        let Some(now) = LocalInstant::try_now() else {
            return ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault);
        };
        let started_at = LocalInstant::from_boot_ns(origin.boot_ns());
        if now < started_at {
            // The execution cannot have started after now on a clock that only
            // moves forward. Saturating to tick zero would silently place every
            // instant of this execution before its own origin.
            return ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault);
        }
        let ticks =
            u64::try_from(now.saturating_duration_since(started_at).as_nanos()).unwrap_or(u64::MAX);
        // The boot clock is monotonic, so this is a defensive latch rather than
        // a correction: a regression means the clock read is untrustworthy.
        let mut last = lock(&self.last_ticks);
        if ticks < *last {
            return ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault);
        }
        *last = ticks;
        ClockReading::Synchronized(RobotInstant::new(origin.timeline(), ticks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::TimelineId;
    use std::time::Duration;

    #[test]
    fn the_real_clock_shares_one_host_wide_domain_across_processes() {
        // Two independently constructed clocks on one origin model two
        // processes on one host: because both read the host boot clock against
        // the same supervisor-minted origin, they compute directly comparable
        // instants - the property the cross-process freshness checks rely on.
        let origin = ExecutionOrigin::mint();
        let a = RealClock::new(origin).expect("current-boot origin");
        let b = RealClock::new(origin).expect("current-boot origin");
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

    #[test]
    fn execution_origins_and_local_instants_share_the_same_boot_clock_scale() {
        let before = LocalInstant::try_now().expect("test host clock");
        let origin = ExecutionOrigin::mint();
        let after = LocalInstant::try_now().expect("test host clock");

        assert!(
            before.boot_ns() <= origin.boot_ns() && origin.boot_ns() <= after.boot_ns(),
            "runtime-contract origin {} escaped the bus clock interval {}..={}",
            origin.boot_ns(),
            before.boot_ns(),
            after.boot_ns()
        );
    }

    #[test]
    fn the_real_clock_never_regresses_within_a_timeline() {
        let clock = RealClock::new(ExecutionOrigin::mint()).expect("current-boot origin");
        let mut last = clock.read().instant().unwrap();
        for _ in 0..1000 {
            let next = clock.read().instant().unwrap();
            assert!(
                next.checked_cmp(last).unwrap() != std::cmp::Ordering::Less,
                "robot time regressed: {next} < {last}"
            );
            last = next;
        }
    }

    #[test]
    fn a_foreign_boot_origin_is_rejected_at_construction() {
        let foreign = ExecutionOrigin::new(
            BootId::from_raw(BootId::current().get() ^ 0xffff),
            LocalInstant::try_now().expect("test host clock").boot_ns(),
            TimelineId::mint(),
        );
        assert_eq!(
            RealClock::new(foreign).expect_err("foreign boot must fail before a clock exists"),
            TimeUnsynchronized::ForeignBoot
        );
    }

    #[test]
    fn an_execution_origin_round_trips_through_the_launch_contract() {
        let origin = ExecutionOrigin::mint();
        assert_eq!(ExecutionOrigin::decode(&origin.encode()), Some(origin));
        assert_eq!(ExecutionOrigin::decode("garbage"), None);
        assert_eq!(
            ExecutionOrigin::decode("1:2:0"),
            None,
            "timeline zero is not a timeline"
        );
        assert_eq!(ExecutionOrigin::decode("1:2:3:4"), None);
    }

    #[test]
    fn the_boot_identity_is_stable_within_one_boot() {
        assert_eq!(BootId::current(), BootId::current());
    }
}
