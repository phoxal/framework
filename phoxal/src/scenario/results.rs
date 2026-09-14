//! Placeholder `ScenarioRun` type for P1.
//!
//! P3 (`capture.rs`, `results.rs`, `report.rs`) replaces this with the
//! typed service histories, selected native samples, and per-action
//! command replies. The shape must already match the planned trait so
//! attribute code can compile today.

/// One completed simulated experiment. P1 stub: empty marker; the type
/// exists so `Scenario::verify` can compile.
#[derive(Debug, Clone, Default)]
pub struct ScenarioRun;
