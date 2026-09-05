//! Simulation-owned contracts.
//!
//! This family carries passive world progress rather than generic runtime telemetry.
//! Its wire spelling is deliberately separate from [`crate::simulator`], which
//! is the Rust host SDK that a concrete world adapter uses.

pub mod api;
