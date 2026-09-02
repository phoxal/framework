//! Black-box coverage of the four contract families and the dynamic tree that
//! declares them.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use phoxal::api as robot;
use phoxal::runtime::api as runtime;
use phoxal::simulation::api as simulation;
use phoxal::supervisor::api as supervisor;

#[path = "api/behavior.rs"]
mod behavior;
#[path = "api/contract_surface.rs"]
mod contract_surface;
#[path = "api/templates.rs"]
mod templates;
#[path = "api/tree.rs"]
mod tree;
#[path = "api/wire_bodies.rs"]
mod wire_bodies;
#[path = "api/wire_invariants.rs"]
mod wire_invariants;

use phoxal::bus::{RobotInstant, TimelineId};

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
