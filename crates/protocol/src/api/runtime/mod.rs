//! The `runtime` contract family: facts every running Phoxal process emits
//! about itself.
//!
//! These contracts describe process-local runtime behavior - diagnostic log
//! events, bus and step telemetry, the authoritative simulation clock - and
//! nothing about who collects them. Any participant may publish here, and any
//! host tool may subscribe; the family names no collector and depends on none.

pub(crate) mod logs;
pub(crate) mod simulation;
pub(crate) mod telemetry;

phoxal_macros::protocol_fragment_group! {
    fragments {
        logs;
        simulation;
        telemetry;
    }
}
