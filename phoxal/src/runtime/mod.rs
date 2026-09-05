//! What a running Phoxal process says about itself.
//!
//! [`api`] is the `runtime` contract family: log events plus bus and step
//! telemetry. Any process publishes here; the family names no collector.

/// The `runtime` contract family.
pub mod api;
