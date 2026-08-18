//! The `runtime` contract family: what a running Phoxal process says about
//! itself.
//!
//! Log events, bus and step telemetry, and the authoritative simulation clock.
//! Any process publishes here; the family names no collector.

crate::nodes! {
    family Runtime;

    logs;
    simulation;
    telemetry;
}
