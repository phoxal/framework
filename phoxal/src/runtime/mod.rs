//! What a running Phoxal process says about itself.
//!
//! [`api`] is the `runtime` contract family: log events, bus and step
//! telemetry, and the authoritative simulation clock. Any process publishes
//! here; the family names no collector.

/// The `runtime` contract family.
pub mod api;
