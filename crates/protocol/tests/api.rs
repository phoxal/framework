//! Black-box coverage of the generated contract-family surface.

#![allow(clippy::expect_used, clippy::unwrap_used)]

pub use phoxal_protocol::*;

#[path = "api/behavior.rs"]
mod behavior;
#[path = "api/contract_surface.rs"]
mod contract_surface;
#[path = "api/family.rs"]
mod family;
#[path = "api/macro_fixtures.rs"]
mod macro_fixtures;
#[path = "api/topic_builder.rs"]
mod topic_builder;
#[path = "api/topic_keys.rs"]
mod topic_keys;
#[path = "api/wire_bodies.rs"]
mod wire_bodies;
#[path = "api/wire_invariants.rs"]
mod wire_invariants;

use phoxal_bus::{RobotInstant, TimelineId};

fn instant(ticks: u64) -> RobotInstant {
    RobotInstant::new(TimelineId::from_raw(1).unwrap(), ticks)
}

fn round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = rmp_serde::to_vec_named(value).unwrap();
    let decoded: T = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(value, &decoded);
}
