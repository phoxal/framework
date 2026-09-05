//! Webots-specific world hosting, native generation, and private controller coordination.
//!
//! This crate is deliberately separate from `phoxal`.
//! It may depend on Webots formats and process behavior without pulling either into the universal
//! framework library or the generic CLI.

pub mod attachment;
pub mod evidence;
pub mod generation;
mod glb;
pub mod lifecycle;
mod obj;
pub mod plan;
pub mod protocol;
pub mod registration;
pub mod robot_generation;
pub mod runtime;
pub mod server;
pub mod state;

/// The exact native controller executable names generated into a Webots project.
pub const WORLD_CONTROLLER_PACKAGE: &str = "phoxal-simulator-webots-world-controller";
pub const ROBOT_CONTROLLER_PACKAGE: &str = "phoxal-simulator-webots-robot-controller";
