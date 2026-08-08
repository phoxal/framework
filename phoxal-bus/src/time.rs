//! The four non-interchangeable time types, and the timeline vocabulary built
//! on them.
//!
//! Each type names one physical fact, and no two of them are convertible, so a
//! caller cannot quietly substitute one for another:
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
//!
//! [`Timed<T>`] pairs a value with the [`RobotInstant`] it belongs to, and
//! [`RetiredTimelines`] records the world histories a process has seen replaced.

use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

use phoxal_runtime_contract::identity::TimelineId;
use serde::{Deserialize, Serialize};

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

/// Set the first time any boot-clock read fails, and never cleared.
///
/// A process that has failed to read its own clock once cannot prove that a
/// later successful read is trustworthy - and the alternative, letting each
/// call site recover on its own, is exactly the silent same-process recovery
/// the failure policy forbids. See [`LocalInstant::clock_faulted`].
static CLOCK_FAULTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl LocalInstant {
    /// Whether this process has ever failed to read the host boot clock.
    ///
    /// Sticky for the life of the process: recovery from a clock fault is a
    /// fresh process, so the runner turns this into ordinary failure rather
    /// than letting the participant quietly carry on once reads start working
    /// again.
    pub fn clock_faulted() -> bool {
        CLOCK_FAULTED.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Read the host's suspend-aware monotonic boot clock, reporting a failed
    /// read instead of hiding it.
    ///
    /// There is deliberately no infallible reader. Any sentinel this could
    /// return is wrong in one direction or the other: zero makes every
    /// retained command look freshly observed, and the far end of the domain
    /// looks fail-closed when it is *checked* but becomes a permanent permit
    /// when it is the instant a deadline is built *from*. So a caller that
    /// cannot read the clock does not get an instant - it stops, drops the
    /// sample, or fails, whichever fails closed for that call site.
    pub fn try_now() -> Option<Self> {
        match read_boot_clock_ns() {
            Some(boot_ns) => Some(LocalInstant { boot_ns }),
            None => {
                CLOCK_FAULTED.store(true, std::sync::atomic::Ordering::Release);
                None
            }
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

/// A checked robot-time operation failed because its operands either belong
/// to different timelines or appear in the wrong direction.
///
/// Timeline identities are equality-only, so a mismatch remains distinct from
/// an ordering error. Callers that need to fail closed on a replaced world can
/// therefore handle [`RobotTimeError::TimelineMismatch`] without confusing it
/// with a producer that supplied a future or reversed instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RobotTimeError {
    /// The operands belong to different world histories and cannot be ordered.
    #[error(transparent)]
    TimelineMismatch(#[from] TimelineMismatch),
    /// The receiver instant precedes its reference instant, so the requested
    /// subtraction or window bounds run in the wrong direction. The field
    /// names are deliberately neutral: for `duration_since`, `receiver` is
    /// `self` and `reference` is the argument; for a window they are the first
    /// and second supplied bounds.
    #[error("robot time is reversed: receiver {receiver} precedes reference {reference}")]
    Reversed {
        /// The instant on the receiving/first-operand side of the operation.
        receiver: RobotInstant,
        /// The instant on the reference/second-operand side of the operation.
        reference: RobotInstant,
    },
}

impl RobotInstant {
    /// Build an instant on `timeline`.
    ///
    /// Framework-internal: participant code obtains a `RobotInstant` from a
    /// runner-minted step token or from an observed sample, never by minting
    /// one. `#[doc(hidden)]` keeps it out of the authoring surface while the
    /// clock drivers and the world authority can still construct it.
    ///
    /// It cannot be `pub(crate)`: the clock drivers live in the `phoxal` crate
    /// and the api tree names this type in body fields, so both need to reach
    /// it. Minting an instant is therefore something a participant can do
    /// deliberately, but not by accident and not through the documented
    /// surface.
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
    /// A future `earlier` instant is a producer/order error, not a zero-length
    /// age. The error remains distinct from [`TimelineMismatch`], which means
    /// the operands came from incomparable world histories.
    pub fn duration_since(self, earlier: RobotInstant) -> Result<Duration, RobotTimeError> {
        self.same_timeline(earlier)?;
        if self.ticks < earlier.ticks {
            return Err(RobotTimeError::Reversed {
                receiver: self,
                reference: earlier,
            });
        }
        Ok(Duration::from_nanos(self.ticks - earlier.ticks))
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
/// Every production instant on the wire is a window: a `Participant::step` publish is a
/// window whose bounds coincide, while a measurement translated from a device
/// clock is honestly wider. Consumers never hand-pick a bound - they ask the
/// named predicates below, each of which answers conservatively for the
/// question it is named after.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
    /// The bounds must share a timeline and be ordered as named. A reversed
    /// pair is rejected: widening a producer bug into an apparently honest
    /// window would hide invalid temporal provenance from every consumer.
    pub fn bounded(earliest: RobotInstant, latest: RobotInstant) -> Result<Self, RobotTimeError> {
        earliest.same_timeline(latest)?;
        if earliest.ticks() > latest.ticks() {
            return Err(RobotTimeError::Reversed {
                receiver: earliest,
                reference: latest,
            });
        }
        Ok(TimeWindow {
            timeline: earliest.timeline(),
            earliest_ticks: earliest.ticks(),
            latest_ticks: latest.ticks(),
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
    ) -> Result<bool, RobotTimeError> {
        if reference.checked_cmp(self.latest())? == std::cmp::Ordering::Less {
            return Ok(false);
        }
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
    ) -> Result<bool, RobotTimeError> {
        if self.latest().checked_cmp(reference)? == std::cmp::Ordering::Greater {
            return Ok(false);
        }
        Ok(reference.duration_since(self.latest())? <= bound)
    }
}

impl<'de> Deserialize<'de> for TimeWindow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawTimeWindow {
            timeline: TimelineId,
            earliest_ticks: u64,
            latest_ticks: u64,
        }

        let raw = RawTimeWindow::deserialize(deserializer)?;
        TimeWindow::bounded(
            RobotInstant::new(raw.timeline, raw.earliest_ticks),
            RobotInstant::new(raw.timeline, raw.latest_ticks),
        )
        .map_err(serde::de::Error::custom)
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

/// A value together with the robot instant it belongs to.
///
/// This is the shared carrier for "some body, as of some point on a timeline":
/// an arbitrated command, a safety verdict, a navigation goal, a perception
/// frame. Every consumer of one asks the same question of it - is this still
/// fresh enough to act on - so that question is answered once, here, rather
/// than re-derived at each call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timed<T> {
    /// The value.
    pub body: T,
    /// The robot instant the value is about.
    pub at: RobotInstant,
}

impl<T> Timed<T> {
    /// Pair `body` with the instant it belongs to.
    pub const fn new(body: T, at: RobotInstant) -> Self {
        Timed { body, at }
    }

    /// Whether this value is recent enough at `now` to act on.
    ///
    /// True when `at` is at or before `now` and no more than `bound` behind it.
    /// A value stamped in `now`'s future is not fresh: it is evidence of a
    /// clock disagreement, and treating it as current would let a producer keep
    /// a stale value alive indefinitely by stamping it forward.
    ///
    /// **A cross-timeline comparison is not fresh.** When `at` and `now` belong
    /// to different world histories there is no ordering between them at all -
    /// timelines are equality-only identities - so the honest answer is "I
    /// cannot say this is fresh", and every caller of this method is deciding
    /// whether to *act*. Failing closed means the world was replaced under a
    /// held value and the value is dropped; failing open would apply a command
    /// from a history that has already ended.
    pub fn fresh_within(&self, now: RobotInstant, bound: Duration) -> bool {
        TimeWindow::exact(self.at)
            .possibly_fresh_within(now, bound)
            .unwrap_or(false)
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
/// Observation time (see [`Observed`](crate::handle::subscriber::Observed)) remains
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

/// The world histories this process has seen replaced.
///
/// Retirement is permanent for the life of the process, and the memory is
/// deliberately unbounded. Timelines are equality-only identities with no
/// ordering, so nothing can recognise an evicted identity as old: a delayed
/// clock from a controller whose timeline had been forgotten would read as a
/// *new* world and reset every participant back into a history that had already
/// ended. The set grows by one identity per world replacement - a reset the
/// operator asked for - so it scales with operator actions, not with traffic.
#[derive(Debug, Default)]
pub struct RetiredTimelines {
    timelines: HashSet<TimelineId>,
}

impl RetiredTimelines {
    /// Whether `timeline` has been retired.
    pub fn contains(&self, timeline: TimelineId) -> bool {
        self.timelines.contains(&timeline)
    }

    /// Record `timeline` as replaced.
    pub fn retire(&mut self, timeline: TimelineId) {
        self.timelines.insert(timeline);
    }

    /// Make `timeline` current again, for the deliberate case where a barrier
    /// is installed on a history this process had already retired.
    pub fn activate(&mut self, timeline: TimelineId) {
        self.timelines.remove(&timeline);
    }
}

/// Read the suspend-aware monotonic boot clock, in nanoseconds since boot.
///
/// Linux `CLOCK_BOOTTIME` and Darwin `CLOCK_MONOTONIC` are the continuous
/// clocks that keep counting across system suspend. Darwin's naming is the
/// trap: its `clock_gettime(3)` documents `CLOCK_MONOTONIC` as continuing to
/// increment while the system is asleep, and it is `CLOCK_UPTIME_RAW` - the
/// `mach_absolute_time` clock that `std::time::Instant` reads - that stops.
///
/// `None` means the read failed. Nothing in the framework converts that into an
/// instant: the clock driver reports it as a clock fault and the participant
/// fails.
fn read_boot_clock_ns() -> Option<u64> {
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
        return None;
    }
    Some(
        u64::try_from(timespec.tv_sec)
            .ok()?
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::try_from(timespec.tv_nsec).ok()?),
    )
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
        let first = LocalInstant::try_now().expect("test host clock");
        let second = LocalInstant::try_now().expect("test host clock");
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
        assert_eq!(
            left.duration_since(right),
            Err(RobotTimeError::TimelineMismatch(TimelineMismatch {
                left: timeline(11),
                right: timeline(12),
            }))
        );
        assert_eq!(
            right.duration_since(left),
            Err(RobotTimeError::TimelineMismatch(TimelineMismatch {
                left: timeline(12),
                right: timeline(11),
            }))
        );
    }

    #[test]
    fn same_timeline_age_is_exact_and_rejects_a_future_reference() {
        let earlier = instant(7, 100);
        let later = instant(7, 350);
        assert_eq!(later.duration_since(earlier), Ok(Duration::from_nanos(250)));
        assert_eq!(
            earlier.duration_since(later),
            Err(RobotTimeError::Reversed {
                receiver: earlier,
                reference: later,
            })
        );
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
    fn bounded_rejects_reversed_bounds_and_mixed_timelines_distinctly() {
        assert_eq!(
            TimeWindow::bounded(instant(3, 600), instant(3, 400)),
            Err(RobotTimeError::Reversed {
                receiver: instant(3, 600),
                reference: instant(3, 400),
            })
        );
        assert_eq!(
            TimeWindow::bounded(instant(3, 400), instant(4, 600)),
            Err(RobotTimeError::TimelineMismatch(TimelineMismatch {
                left: timeline(3),
                right: timeline(4),
            }))
        );
    }

    #[test]
    fn time_window_deserialization_rejects_reversed_bounds() {
        let json = serde_json::json!({
            "timeline": 3,
            "earliest_ticks": 600,
            "latest_ticks": 400,
        });
        assert!(serde_json::from_value::<TimeWindow>(json).is_err());

        #[derive(Serialize)]
        struct RawTimeWindow {
            timeline: TimelineId,
            earliest_ticks: u64,
            latest_ticks: u64,
        }

        let bytes = rmp_serde::to_vec_named(&RawTimeWindow {
            timeline: timeline(3),
            earliest_ticks: 600,
            latest_ticks: 400,
        })
        .expect("malformed test fixture should encode");
        assert!(rmp_serde::from_slice::<TimeWindow>(&bytes).is_err());
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

    #[test]
    fn a_timed_value_is_fresh_only_within_the_bound_and_never_from_the_future() {
        let now = instant(1, 1_000);

        let fresh = Timed::new("go", instant(1, 900));
        assert!(fresh.fresh_within(now, Duration::from_nanos(100)));
        assert!(
            fresh.fresh_within(now, Duration::from_nanos(1_000)),
            "a wider bound admits the same value"
        );

        // The bound is reached *at* the bound, matching every other freshness
        // gate in the framework, so no value is live in one layer and dead in
        // the next.
        let stale = Timed::new("go", instant(1, 400));
        assert!(stale.fresh_within(now, Duration::from_nanos(600)));
        assert!(!stale.fresh_within(now, Duration::from_nanos(599)));

        // A stamp in the reference's future is evidence of clock disagreement,
        // not of freshness.
        let future = Timed::new("go", instant(1, 1_001));
        assert!(!future.fresh_within(now, Duration::from_secs(1)));
    }

    #[test]
    fn a_timed_value_from_another_world_history_is_never_fresh() {
        let now = instant(1, 1_000);
        // Identical ticks, different timeline: there is no ordering between
        // them, so the gate fails closed rather than reading the tick numbers
        // as if they were comparable.
        let foreign = Timed::new("go", instant(2, 1_000));
        assert!(!foreign.fresh_within(now, Duration::from_secs(1)));
        assert!(!foreign.fresh_within(now, Duration::ZERO));
    }

    /// A ring of retired timelines would hand a delayed clock from a forgotten
    /// controller back its status as a brand-new world.
    #[test]
    fn a_retired_timeline_is_never_forgotten() {
        let mut retired = RetiredTimelines::default();
        let first = TimelineId::mint();
        retired.retire(first);
        for _ in 0..64 {
            retired.retire(TimelineId::mint());
        }
        assert!(
            retired.contains(first),
            "an old world must still be recognised as retired after many resets"
        );

        retired.activate(first);
        assert!(
            !retired.contains(first),
            "a deliberately reactivated timeline is no longer retired"
        );
    }
}
