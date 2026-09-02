//! The supervisor-owned execution time domain.
//!
//! A stream carries ordered replacements and `current` closes the subscribe
//! race. `revision` orders supervisor state updates, while `timeline` is an
//! opaque identity for one history and has no ordering relation to another.

crate::endpoints! {
    self: Stream<TimeDomainStream, Out>;
    current: Query<CurrentRequest, CurrentResponse>;
}

use crate::identity::TimelineId;

/// The cadence source an execution currently authorizes for services and the
/// brain.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TimeMode {
    /// Services and the brain schedule from the host-local monotonic clock.
    Monotonic,
    /// Services and the brain advance only from `simulation/clock`.
    Simulated,
}

/// The supervisor's complete current scheduling authority.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct TimeDomain {
    /// Strictly increasing supervisor state revision within one execution.
    pub revision: u64,
    /// The opaque history newly active at this revision.
    pub timeline: TimelineId,
    /// The scheduling source the active history uses.
    pub mode: TimeMode,
}

/// Ask for the current complete domain after subscribing to its update stream.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct CurrentRequest {}

/// The complete value returned by [`CurrentRequest`].
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct CurrentResponse {
    /// The current supervisor-owned scheduling authority.
    pub domain: TimeDomain,
}

/// A complete replacement published on the ordered domain stream.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct TimeDomainStream {
    /// The replacement scheduling authority.
    pub domain: TimeDomain,
}
