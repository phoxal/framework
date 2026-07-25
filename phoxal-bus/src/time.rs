//! The four non-interchangeable time types (#952 section C).
//!
//! Phoxal used to represent several incomparable physical facts with one
//! `LogicalTime` that any caller could mint. This module replaces it with types
//! that cannot be confused for one another:
//!
//! - [`LocalInstant`] - a reading of this host's suspend-aware monotonic boot
//!   clock. Process-local in meaning, host-wide in domain, and **never
//!   serialized**.
//! - [`RobotInstant`] - an exact instant on one [`TimelineId`]. Comparison and
//!   age are checked operations that fail across timelines.
//! - [`TimeWindow`] - a bounded, possibly asymmetric estimate of a
//!   [`RobotInstant`]. It never silently collapses into an exact instant.
//! - [`WallTimestamp`] - calendar diagnostics only. It implements no ordering,
//!   no arithmetic, and no freshness interface, and no checked publisher or
//!   capture API accepts it.
//!
//! There is no ordering or arithmetic *across* these types, and none of them
//! has a "zero means absent" sentinel: absence of a production instant is
//! represented as `Option::None`.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::identity::TimelineId;

/// Comparing or subtracting instants that belong to different world histories.
///
/// Two timelines are opaque identities with no generation order, so the answer
/// is not "some large number" - there is no answer at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("robot instants belong to different timelines ({left} vs {right})")]
pub struct TimelineMismatch {
    /// The timeline of the left-hand operand.
    pub left: TimelineId,
    /// The timeline of the right-hand operand.
    pub right: TimelineId,
}

/// A reading of the host's suspend-aware monotonic boot clock.
///
/// This is the authoritative host clock for every liveness decision in the
/// framework: command-silence deadlines, actuator permits, and bus-stamped
/// observation. It is *not* robot time and carries no timeline.
///
/// **Suspend counts.** On Linux this reads `CLOCK_BOOTTIME` and on macOS
/// `CLOCK_MONOTONIC` (Darwin's monotonic clock is the continuous one; its
/// `CLOCK_UPTIME_RAW` is the variant that stops). `std::time::Instant` reads the
/// *stopping* clock on both platforms, so a host that suspended for an hour
/// would resume treating a retained command as fresh. Every control path uses
/// this type instead.
///
/// The domain is host-wide: two processes on one host obtain directly
/// comparable readings. The values are still never serialized - a reading is
/// meaningless on another host, so putting one on the wire would be exactly the
/// category error this module exists to delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalInstant {
    boot_ns: u64,
}

impl LocalInstant {
    /// Read the host's suspend-aware monotonic boot clock.
    pub fn now() -> Self {
        LocalInstant {
            boot_ns: boot_clock_ns(),
        }
    }

    /// Nanoseconds since host boot.
    ///
    /// Exposed for the execution origin the supervisor mints and for
    /// diagnostics. It is deliberately not a `From`/`Into` conversion: a bare
    /// integer is not a time type.
    pub const fn boot_ns(self) -> u64 {
        self.boot_ns
    }

    /// Reconstruct a reading from a boot-clock nanosecond value.
    #[doc(hidden)]
    pub const fn from_boot_ns(boot_ns: u64) -> Self {
        LocalInstant { boot_ns }
    }

    /// How long ago `earlier` was, saturating at zero if it is in the future.
    pub fn saturating_duration_since(self, earlier: LocalInstant) -> Duration {
        Duration::from_nanos(self.boot_ns.saturating_sub(earlier.boot_ns))
    }

    /// This instant advanced by `delta`, saturating at the end of the domain.
    pub fn saturating_add(self, delta: Duration) -> Self {
        LocalInstant {
            boot_ns: self
                .boot_ns
                .saturating_add(u64::try_from(delta.as_nanos()).unwrap_or(u64::MAX)),
        }
    }

    /// Whether `self` is at or past `deadline`.
    pub fn reached(self, deadline: LocalInstant) -> bool {
        self.boot_ns >= deadline.boot_ns
    }
}

/// An exact instant on one world history.
///
/// A tick is one nanosecond. The origin is the timeline's own: for a real
/// execution it is the supervisor-minted execution origin on the host boot
/// clock, and for a simulated or replayed timeline it is the world authority's
/// zero. Two `RobotInstant`s are only meaningful together when their
/// [`TimelineId`]s are equal, so comparison and age are checked operations -
/// there is no `Ord` and no `Sub`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RobotInstant {
    timeline: TimelineId,
    ticks: u64,
}

impl RobotInstant {
    /// Build an instant on `timeline`.
    ///
    /// Framework-internal: participant code obtains a `RobotInstant` from a
    /// runner-minted step token or from an observed sample, never by minting
    /// one. `#[doc(hidden)]` keeps it out of the authoring surface while the
    /// clock drivers and the world authority can still construct it.
    #[doc(hidden)]
    pub const fn new(timeline: TimelineId, ticks: u64) -> Self {
        RobotInstant { timeline, ticks }
    }

    /// The world history this instant belongs to.
    pub const fn timeline(self) -> TimelineId {
        self.timeline
    }

    /// Ticks (nanoseconds) since this timeline's origin.
    pub const fn ticks(self) -> u64 {
        self.ticks
    }

    /// Order this instant against another on the same timeline.
    pub fn checked_cmp(self, other: RobotInstant) -> Result<std::cmp::Ordering, TimelineMismatch> {
        self.same_timeline(other)?;
        Ok(self.ticks.cmp(&other.ticks))
    }

    /// How long after `earlier` this instant is, on the same timeline.
    ///
    /// Saturates at zero when `earlier` is later, so a caller never sees a
    /// wrapped duration; use [`checked_cmp`](Self::checked_cmp) when the
    /// direction itself matters.
    pub fn duration_since(self, earlier: RobotInstant) -> Result<Duration, TimelineMismatch> {
        self.same_timeline(earlier)?;
        Ok(Duration::from_nanos(
            self.ticks.saturating_sub(earlier.ticks),
        ))
    }

    /// This instant advanced by `delta`, saturating at the end of the timeline.
    #[must_use]
    pub fn saturating_add(self, delta: Duration) -> Self {
        RobotInstant {
            timeline: self.timeline,
            ticks: self
                .ticks
                .saturating_add(u64::try_from(delta.as_nanos()).unwrap_or(u64::MAX)),
        }
    }

    fn same_timeline(self, other: RobotInstant) -> Result<(), TimelineMismatch> {
        if self.timeline == other.timeline {
            Ok(())
        } else {
            Err(TimelineMismatch {
                left: self.timeline,
                right: other.timeline,
            })
        }
    }
}

impl fmt::Display for RobotInstant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.timeline, self.ticks)
    }
}

/// A bounded estimate of a [`RobotInstant`], with possibly asymmetric bounds.
///
/// Every production instant on the wire is a window: a `#[step]` publish is a
/// window whose bounds coincide, while a measurement translated from a device
/// clock is honestly wider. Consumers never hand-pick a bound - they ask the
/// named predicates below, each of which answers conservatively for the
/// question it is named after.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    timeline: TimelineId,
    earliest_ticks: u64,
    latest_ticks: u64,
}

impl TimeWindow {
    /// A window with coincident bounds: the instant is known exactly.
    pub const fn exact(instant: RobotInstant) -> Self {
        TimeWindow {
            timeline: instant.timeline(),
            earliest_ticks: instant.ticks(),
            latest_ticks: instant.ticks(),
        }
    }

    /// A window spanning `[earliest, latest]`.
    ///
    /// The bounds must share a timeline; a reversed pair is normalized rather
    /// than rejected, because an estimator that produced one has a bug the
    /// consumer cannot fix and a wider honest window is the safe reading.
    pub fn bounded(earliest: RobotInstant, latest: RobotInstant) -> Result<Self, TimelineMismatch> {
        earliest.same_timeline(latest)?;
        Ok(TimeWindow {
            timeline: earliest.timeline(),
            earliest_ticks: earliest.ticks().min(latest.ticks()),
            latest_ticks: earliest.ticks().max(latest.ticks()),
        })
    }

    /// The world history this estimate belongs to.
    pub const fn timeline(self) -> TimelineId {
        self.timeline
    }

    /// The earliest instant this estimate admits.
    pub const fn earliest(self) -> RobotInstant {
        RobotInstant::new(self.timeline, self.earliest_ticks)
    }

    /// The latest instant this estimate admits.
    pub const fn latest(self) -> RobotInstant {
        RobotInstant::new(self.timeline, self.latest_ticks)
    }

    /// The exact instant, if this estimate has coincident bounds.
    ///
    /// Deliberately fallible: a window never silently collapses into an exact
    /// instant.
    pub const fn as_exact(self) -> Option<RobotInstant> {
        if self.earliest_ticks == self.latest_ticks {
            Some(RobotInstant::new(self.timeline, self.earliest_ticks))
        } else {
            None
        }
    }

    /// The width of this estimate.
    pub const fn uncertainty(self) -> Duration {
        Duration::from_nanos(self.latest_ticks - self.earliest_ticks)
    }

    /// Whether this estimate is precise enough for a consuming contract's
    /// bound.
    pub fn uncertainty_within(self, bound: Duration) -> bool {
        self.uncertainty() <= bound
    }

    /// Whether *every* instant this estimate admits is older than
    /// `reference - bound`.
    ///
    /// This is the fail-closed staleness question: it is true only when
    /// staleness is certain.
    pub fn definitely_older_than(
        self,
        reference: RobotInstant,
        bound: Duration,
    ) -> Result<bool, TimelineMismatch> {
        Ok(reference.duration_since(self.latest())? > bound)
    }

    /// Whether *some* instant this estimate admits is within `bound` of
    /// `reference`, and none of them is in `reference`'s future.
    ///
    /// This is the admissibility question a freshness gate asks. Both halves
    /// read the bound that actually decides them: "none in the future" is about
    /// the **latest** instant the window admits, and "some within reach" is
    /// about that same latest instant, since it is the newest candidate. Asking
    /// either question of `earliest` gets both wrong - it would admit a window
    /// straddling the future, and reject a wide window whose newest instant is
    /// perfectly recent.
    pub fn possibly_fresh_within(
        self,
        reference: RobotInstant,
        bound: Duration,
    ) -> Result<bool, TimelineMismatch> {
        if self.latest().checked_cmp(reference)? == std::cmp::Ordering::Greater {
            return Ok(false);
        }
        Ok(reference.duration_since(self.latest())? <= bound)
    }
}

impl fmt::Display for TimeWindow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_exact() {
            Some(exact) => write!(formatter, "{exact}"),
            None => write!(
                formatter,
                "{}@[{}..{}]",
                self.timeline, self.earliest_ticks, self.latest_ticks
            ),
        }
    }
}

/// When a sensor observation was captured, as honestly as the driver can say.
///
/// A driver owns mapping its device clock into robot time - including reset,
/// drift, wraparound, batching, and exposure-versus-readout semantics. When it
/// can do that, it says so with a [`TimeWindow`] whose width is the real
/// uncertainty; when it cannot, it says *that*, rather than inventing an
/// instant that a consumer would then trust.
///
/// Observation time (see [`Observed`](crate::handle::Observed)) remains
/// available for detecting transport silence and does not replace capture time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureStamp {
    /// Capture translated into robot time, with honest bounds.
    Translated(TimeWindow),
    /// The device clock could not be translated into robot time. The sample is
    /// still worth publishing; it just carries no production instant, so no
    /// consumer can mistake it for one.
    Untranslated,
}

impl CaptureStamp {
    /// An exactly known capture instant.
    pub const fn exact(instant: RobotInstant) -> Self {
        CaptureStamp::Translated(TimeWindow::exact(instant))
    }

    /// The production instant this stamp puts on the wire.
    pub(crate) const fn into_window(self) -> Option<TimeWindow> {
        match self {
            CaptureStamp::Translated(window) => Some(window),
            CaptureStamp::Untranslated => None,
        }
    }
}

/// A calendar timestamp, for diagnostics only.
///
/// It implements no ordering, no arithmetic, and no freshness interface, it is
/// accepted by no checked publisher or capture API, and it appears in no
/// official control decision. Its only purpose is to give a human reading a log
/// line a date.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallTimestamp {
    unix_ns: u64,
}

impl WallTimestamp {
    /// Read the host calendar clock.
    pub fn now() -> Self {
        let unix_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| u64::try_from(since.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        WallTimestamp { unix_ns }
    }

    /// Nanoseconds since the UNIX epoch, for a formatter.
    pub const fn unix_ns(self) -> u64 {
        self.unix_ns
    }
}

impl fmt::Display for WallTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}ns since the UNIX epoch", self.unix_ns)
    }
}

/// Read the suspend-aware monotonic boot clock, in nanoseconds since boot.
///
/// Linux `CLOCK_BOOTTIME` and Darwin `CLOCK_MONOTONIC` are the continuous
/// clocks that keep counting across system suspend. A failed read is a clock
/// fault the caller cannot paper over, but returning an error from
/// `LocalInstant::now()` would poison every call site; the clock-discipline
/// supervisor detects a stuck clock instead (see the `TimeUnsynchronized`
/// triggers), so this saturates rather than panicking.
fn boot_clock_ns() -> u64 {
    #[cfg(target_os = "linux")]
    const CLOCK: libc::clockid_t = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;

    let mut timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes into the `timespec` we own and borrow
    // mutably for the duration of the call; both clock ids are supported on
    // their respective targets.
    let outcome = unsafe { libc::clock_gettime(CLOCK, &raw mut timespec) };
    if outcome != 0 {
        return 0;
    }
    u64::try_from(timespec.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(timespec.tv_nsec).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline(value: u64) -> TimelineId {
        TimelineId::from_raw(value).expect("test timeline id must be nonzero")
    }

    fn instant(line: u64, ticks: u64) -> RobotInstant {
        RobotInstant::new(timeline(line), ticks)
    }

    #[test]
    fn local_instant_reads_a_monotonic_host_wide_domain() {
        let first = LocalInstant::now();
        let second = LocalInstant::now();
        assert!(second >= first);
        assert!(first.boot_ns() > 0, "the boot clock must be readable");
        // Two independently taken readings model two processes on one host:
        // the domain is shared, so they are directly comparable.
        assert!(second.saturating_duration_since(first) < Duration::from_secs(1));
        assert_eq!(
            first.saturating_duration_since(second),
            Duration::ZERO,
            "a future reference saturates instead of wrapping"
        );
    }

    #[test]
    fn cross_timeline_comparison_is_a_checked_error_in_both_directions() {
        let left = instant(11, 100);
        let right = instant(12, 100);
        assert_eq!(
            left.checked_cmp(right),
            Err(TimelineMismatch {
                left: timeline(11),
                right: timeline(12)
            })
        );
        assert_eq!(
            right.checked_cmp(left),
            Err(TimelineMismatch {
                left: timeline(12),
                right: timeline(11)
            })
        );
        assert!(left.duration_since(right).is_err());
        assert!(right.duration_since(left).is_err());
    }

    #[test]
    fn same_timeline_age_is_exact_and_saturates_on_a_future_reference() {
        let earlier = instant(7, 100);
        let later = instant(7, 350);
        assert_eq!(later.duration_since(earlier), Ok(Duration::from_nanos(250)));
        assert_eq!(earlier.duration_since(later), Ok(Duration::ZERO));
        assert_eq!(later.checked_cmp(earlier), Ok(std::cmp::Ordering::Greater));
    }

    #[test]
    fn an_exact_window_round_trips_and_a_bounded_one_does_not_collapse() {
        let exact = TimeWindow::exact(instant(3, 500));
        assert_eq!(exact.as_exact(), Some(instant(3, 500)));
        assert_eq!(exact.uncertainty(), Duration::ZERO);

        let bounded = TimeWindow::bounded(instant(3, 400), instant(3, 600)).unwrap();
        assert_eq!(bounded.as_exact(), None);
        assert_eq!(bounded.uncertainty(), Duration::from_nanos(200));
        assert!(bounded.uncertainty_within(Duration::from_nanos(200)));
        assert!(!bounded.uncertainty_within(Duration::from_nanos(199)));
    }

    #[test]
    fn bounded_normalizes_reversed_bounds_and_rejects_mixed_timelines() {
        let reversed = TimeWindow::bounded(instant(3, 600), instant(3, 400)).unwrap();
        assert_eq!(reversed.earliest(), instant(3, 400));
        assert_eq!(reversed.latest(), instant(3, 600));
        assert!(TimeWindow::bounded(instant(3, 400), instant(4, 600)).is_err());
    }

    #[test]
    fn freshness_predicates_are_conservative_at_both_ends() {
        let reference = instant(1, 1_000);
        let window = TimeWindow::bounded(instant(1, 400), instant(1, 600)).unwrap();

        // Certain staleness needs the *whole* window to be older.
        assert!(
            window
                .definitely_older_than(reference, Duration::from_nanos(399))
                .unwrap()
        );
        assert!(
            !window
                .definitely_older_than(reference, Duration::from_nanos(400))
                .unwrap()
        );

        // Possible freshness is decided by the newest instant the window
        // admits: `[400,600]` against reference 1000 is 400ns old at best.
        assert!(
            window
                .possibly_fresh_within(reference, Duration::from_nanos(400))
                .unwrap()
        );
        assert!(
            !window
                .possibly_fresh_within(reference, Duration::from_nanos(399))
                .unwrap()
        );

        // A wide window whose newest instant is recent is still usable; asking
        // the earliest bound instead would reject it.
        let wide = TimeWindow::bounded(instant(1, 0), instant(1, 950)).unwrap();
        assert!(
            wide.possibly_fresh_within(reference, Duration::from_nanos(100))
                .unwrap()
        );

        // A window that straddles the reference's future is never usable as a
        // fresh past observation, even though its earliest bound is past.
        let straddling = TimeWindow::bounded(instant(1, 900), instant(1, 1_100)).unwrap();
        assert!(
            !straddling
                .possibly_fresh_within(reference, Duration::from_secs(1))
                .unwrap()
        );
        let wholly_future = TimeWindow::bounded(instant(1, 1_001), instant(1, 1_100)).unwrap();
        assert!(
            !wholly_future
                .possibly_fresh_within(reference, Duration::from_secs(1))
                .unwrap()
        );

        // And both predicates refuse to answer across timelines.
        let foreign = TimeWindow::exact(instant(2, 400));
        assert!(
            foreign
                .definitely_older_than(reference, Duration::ZERO)
                .is_err()
        );
        assert!(
            foreign
                .possibly_fresh_within(reference, Duration::ZERO)
                .is_err()
        );
    }
}
