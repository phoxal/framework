//! Golden tests for the generated API layer.
//!
//! These are unit tests rather than integration tests for one concrete reason:
//! the curation tests in [`revision`] read `API_CONTRACT_MANIFEST`, the tree's
//! own `#[cfg(test)]` self-enumeration, which `phoxal_api_tree!` deliberately
//! emits only into test builds so it never becomes public API surface. An
//! integration test target compiles this crate without `cfg(test)` and cannot
//! see it, so moving these out would mean either dropping the curation tests or
//! widening generated public API to suit the test layout. Everything else here
//! exercises public API and is placed alongside them so one concern is not split
//! across two targets.
//!
//! Because `lib.rs` is a single macro invocation, its "modules" are generated
//! and an inline `mod tests` per module is impossible; the files below are split
//! by the concern they pin instead.

mod behavior;
mod macro_fixtures;
mod revision;
mod topic_builder;
mod topic_keys;
mod wire_bodies;

use phoxal_bus::{RobotInstant, TimelineId};

/// A same-timeline instant for round-trip fixtures.
fn instant(ticks: u64) -> RobotInstant {
    RobotInstant::new(TimelineId::from_raw(1).unwrap(), ticks)
}

/// Encode and decode `value` the way the bus does, and assert it survives.
fn round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = rmp_serde::to_vec_named(value).unwrap();
    let decoded: T = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(value, &decoded);
}
