//! Re-exports the simulation-clock wire contract from `phoxal-core-engine`.
//!
//! The contract type, topic, and builder helpers live in
//! `crate::runtime::sim_clock`. This module is kept only so existing
//! consumers writing `crate::api::simulation::v1::clock::*` keep compiling.

pub use crate::runtime::sim_clock::{
    SimulationClock as Clock, TOPIC, publisher, publisher_builder, subscriber_builder, topic,
};
