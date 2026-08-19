//! The external simulation hand-off: the authoritative world clock.
//!
//! The endpoint's semantic is `WorldClock`, a sibling of `State` rather than a
//! subtype of it: that is what keeps the ordinary state publisher every
//! participant has from minting world steps, so only the dedicated world-clock
//! publisher - which no participant reaches - can take this endpoint. Its wire
//! kind stays `Event`: the hand is stamped at a completed world step, and its
//! ordered stream transport preserves every accepted clock and reports gaps
//! instead of silently coalescing them.

crate::endpoints! {
    clock: WorldClock<Clock>;
}

/// The body carried by the authoritative simulation clock hand.
///
/// The production timeline and exact instant are bus metadata. The body carries
/// only the simulator's monotonic step counter.
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
pub struct Clock {
    pub step: u64,
}
