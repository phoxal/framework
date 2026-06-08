//! # phoxal
//!
//! The Phoxal robot framework as one crate. Production-oriented autonomous
//! mobile robots: manifest-driven, simulation-first, typed bus.
//!
//! Module map:
//! - [`bus`] — typed Zenoh transport + pubsub/query leaves.
//! - [`model`] — authored manifest schemas (`robot.yaml`, `structure.urdf`,
//!   `component.yaml`, `simulation.yaml`).
//! - [`spatial`] — geometry, frames, transforms, risk model.
//! - [`api`] — typed bus wire contracts, one module per platform runtime.
//! - [`runtime`] — the `Runtime` trait, bootstrap, logical clock, runtime context.
//! - [`scenario`] — Webots-backed validation harness (feature `scenario`).
//! - [`util`] — shared helpers.

pub mod api;
pub mod bus;
pub mod model;
pub mod runtime;
pub mod spatial;
pub mod util;

#[cfg(feature = "scenario")]
pub mod scenario;
