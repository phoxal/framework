//! Black-box coverage of the generated contract-family surface.

#![allow(clippy::expect_used, clippy::unwrap_used)]

// The protocol tree is a private module of `phoxal`; the generated catalogue and
// its contract surface reach this test through the hidden `__compat` re-export
// that exists for exactly that reason.
use phoxal::__compat::{API_CONTRACT_MANIFEST, ApiContractManifestFamily, protocol as __compat};
use phoxal::api as robot;
use phoxal::runtime::api as runtime;
use phoxal::supervisor::api as supervisor;

#[path = "api/behavior.rs"]
mod behavior;
#[path = "api/contract_surface.rs"]
mod contract_surface;
#[path = "api/family.rs"]
mod family;
#[path = "api/topic_builder.rs"]
mod topic_builder;
#[path = "api/topic_keys.rs"]
mod topic_keys;
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
