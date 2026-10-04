//! Two-phase hardware release selection.
//!
//! A candidate records the actual input-freeze instant and newest due nominal
//! release. The schedule advances only after the runtime invocation is
//! accepted, so failed candidates never consume an invocation index.

use super::{ExecutionDuration, ExecutionTime, StepContext};

/// One selected hardware invocation awaiting acceptance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HardwareInvocation {
    context: StepContext,
    input_boundary: u64,
    nominal_release: ExecutionTime,
    following_release: ExecutionTime,
}

impl HardwareInvocation {
    /// Build an invocation candidate supplied by a supervisor-controlled
    /// boundary.  Controlled execution does not advance this hardware
    /// schedule; it reuses the same input/output adapter contract with an
    /// explicit logical timestamp and no missed releases.
    pub(crate) const fn controlled(context: StepContext, boundary: u64) -> Self {
        Self {
            context,
            input_boundary: boundary,
            nominal_release: context.now(),
            following_release: context.now(),
        }
    }

    /// Runtime facts for the frozen input cut.
    #[must_use]
    pub const fn context(self) -> StepContext {
        self.context
    }

    /// Input eligibility fence, distinct from the service invocation count.
    #[must_use]
    pub const fn input_boundary(self) -> u64 {
        self.input_boundary
    }

    /// Newest due nominal release selected for this invocation.
    #[must_use]
    pub const fn nominal_release(self) -> ExecutionTime {
        self.nominal_release
    }
}

/// Hardware cadence owner using one execution-monotonic clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HardwareSchedule {
    period: ExecutionDuration,
    next_release: ExecutionTime,
    previous_freeze: Option<ExecutionTime>,
    next_index: u64,
}

impl HardwareSchedule {
    /// Anchor a runtime's first nominal release at initialization.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError::ZeroPeriod`] for a zero cadence.
    pub const fn new(
        anchor: ExecutionTime,
        period: ExecutionDuration,
    ) -> Result<Self, ScheduleError> {
        if period.as_nanos() == 0 {
            return Err(ScheduleError::ZeroPeriod);
        }
        Ok(Self {
            period,
            next_release: anchor,
            previous_freeze: None,
            next_index: 0,
        })
    }

    /// Select the newest due nominal release without advancing the schedule.
    ///
    /// `now` is the actual input-freeze instant. Older due releases are skipped
    /// and reported, never replayed as catch-up invocations.
    ///
    /// # Errors
    ///
    /// Returns a typed not-due, reversed-clock, or arithmetic failure.
    pub fn candidate(&self, now: ExecutionTime) -> Result<HardwareInvocation, ScheduleError> {
        if self.previous_freeze.is_some_and(|previous| now < previous) {
            return Err(ScheduleError::ClockReversed);
        }
        if now < self.next_release {
            return Err(ScheduleError::NotDue {
                next_release: self.next_release,
            });
        }
        let overdue = now.as_nanos() - self.next_release.as_nanos();
        let missed_releases = overdue / self.period.as_nanos();
        let skipped = self
            .period
            .as_nanos()
            .checked_mul(missed_releases)
            .ok_or(ScheduleError::TimeOverflow)?;
        let nominal_nanos = self
            .next_release
            .as_nanos()
            .checked_add(skipped)
            .ok_or(ScheduleError::TimeOverflow)?;
        let following_nanos = nominal_nanos
            .checked_add(self.period.as_nanos())
            .ok_or(ScheduleError::TimeOverflow)?;
        let elapsed = match self.previous_freeze {
            Some(previous) => ExecutionDuration::from_nanos(now.as_nanos() - previous.as_nanos()),
            None => ExecutionDuration::from_nanos(0),
        };
        Ok(HardwareInvocation {
            context: StepContext::new(now, self.period, elapsed, missed_releases, self.next_index),
            input_boundary: self.next_index,
            nominal_release: ExecutionTime::from_nanos(nominal_nanos),
            following_release: ExecutionTime::from_nanos(following_nanos),
        })
    }

    /// Commit a selected release after its complete invocation is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError::StaleCandidate`] if the candidate does not
    /// describe this schedule's current next release and invocation index.
    pub fn accept(&mut self, candidate: HardwareInvocation) -> Result<(), ScheduleError> {
        let context = candidate.context;
        if context.invocation_index() != self.next_index
            || candidate.nominal_release < self.next_release
            || !(candidate.nominal_release.as_nanos() - self.next_release.as_nanos())
                .is_multiple_of(self.period.as_nanos())
            || candidate.following_release.as_nanos()
                != candidate
                    .nominal_release
                    .as_nanos()
                    .checked_add(self.period.as_nanos())
                    .ok_or(ScheduleError::TimeOverflow)?
        {
            return Err(ScheduleError::StaleCandidate);
        }
        self.next_release = candidate.following_release;
        self.previous_freeze = Some(context.now());
        self.next_index = self
            .next_index
            .checked_add(1)
            .ok_or(ScheduleError::InvocationOverflow)?;
        Ok(())
    }

    /// Next nominal release that has not been accepted or skipped.
    #[must_use]
    pub const fn next_release(self) -> ExecutionTime {
        self.next_release
    }

    /// Next accepted invocation index.
    #[must_use]
    pub const fn next_invocation_index(self) -> u64 {
        self.next_index
    }
}

/// Hardware release selection failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScheduleError {
    /// A runtime cannot schedule at a zero cadence.
    #[error("hardware runtime period must be positive")]
    ZeroPeriod,
    /// The input-freeze instant precedes the next due release.
    #[error("hardware runtime is not due until {next_release:?}")]
    NotDue { next_release: ExecutionTime },
    /// The execution-monotonic clock moved behind the prior input freeze.
    #[error("hardware execution clock moved backwards")]
    ClockReversed,
    /// Nanosecond release arithmetic overflowed.
    #[error("hardware release time overflowed")]
    TimeOverflow,
    /// The accepted invocation counter overflowed.
    #[error("hardware invocation counter overflowed")]
    InvocationOverflow,
    /// A candidate was already consumed or came from another schedule state.
    #[error("hardware invocation candidate is stale")]
    StaleCandidate,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_hardware_invocations_match_the_normative_timing_table() {
        let period = ExecutionDuration::from_millis(20);
        let mut schedule =
            HardwareSchedule::new(ExecutionTime::from_nanos(0), period).expect("valid schedule");

        let first = schedule
            .candidate(ExecutionTime::from_nanos(7_000_000))
            .expect("first due");
        assert_eq!(first.nominal_release().as_nanos(), 0);
        assert_eq!(first.context().elapsed().as_millis(), 0);
        assert_eq!(first.context().missed_releases(), 0);
        schedule.accept(first).expect("first accepted");

        let second = schedule
            .candidate(ExecutionTime::from_nanos(68_000_000))
            .expect("late due");
        assert_eq!(second.nominal_release().as_nanos(), 60_000_000);
        assert_eq!(second.context().elapsed().as_millis(), 61);
        assert_eq!(second.context().missed_releases(), 2);
        schedule.accept(second).expect("second accepted");

        let third = schedule
            .candidate(ExecutionTime::from_nanos(81_000_000))
            .expect("next due");
        assert_eq!(third.nominal_release().as_nanos(), 80_000_000);
        assert_eq!(third.context().elapsed().as_millis(), 13);
        assert_eq!(third.context().missed_releases(), 0);
    }

    #[test]
    fn candidates_do_not_advance_until_acceptance() {
        let schedule = HardwareSchedule::new(
            ExecutionTime::from_nanos(0),
            ExecutionDuration::from_millis(10),
        )
        .expect("valid schedule");
        let first = schedule
            .candidate(ExecutionTime::from_nanos(25_000_000))
            .expect("due");
        let repeated = schedule
            .candidate(ExecutionTime::from_nanos(25_000_000))
            .expect("still due");
        assert_eq!(first, repeated);
        assert_eq!(schedule.next_invocation_index(), 0);
    }

    #[test]
    fn not_due_and_reversed_clocks_are_distinct() {
        let mut schedule = HardwareSchedule::new(
            ExecutionTime::from_nanos(10),
            ExecutionDuration::from_nanos(10),
        )
        .expect("valid schedule");
        assert!(matches!(
            schedule.candidate(ExecutionTime::from_nanos(9)),
            Err(ScheduleError::NotDue { .. })
        ));
        let first = schedule
            .candidate(ExecutionTime::from_nanos(10))
            .expect("due");
        schedule.accept(first).expect("accept");
        assert_eq!(
            schedule.candidate(ExecutionTime::from_nanos(9)),
            Err(ScheduleError::ClockReversed)
        );
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    #[test]
    fn controlled_input_boundary_is_independent_of_service_invocation_count() {
        let context = StepContext::new(
            ExecutionTime::from_nanos(20_000_000),
            ExecutionDuration::from_nanos(20_000_000),
            ExecutionDuration::from_nanos(20_000_000),
            0,
            1,
        );
        let invocation = HardwareInvocation::controlled(context, 10);
        assert_eq!(invocation.input_boundary(), 10);
        assert_eq!(invocation.context().invocation_index(), 1);
    }
}
