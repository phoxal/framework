//! Shared serialized artifact format.
//!
//! This module owns the inert data records, format constants, and pure
//! validation/digest functions that the framework uses to communicate
//! artifact and bundle shape across the compiler, simulator, supervisor,
//! and SDK boundaries.
//!
//! It must not discover a project, inspect native executable sections,
//! walk a directory, invoke Cargo, open a network connection, or start a
//! process. Anything with those concerns belongs to `cargo-phoxal` or
//! to the simulator/supervisor/SDK application that consumes the format.
//!
//! ## Submodules
//!
//! - This module's root owns `RuntimeRecord`, `InputRecord`, `OutputRecord`,
//!   `PortKind`, `InputKind`, `OutputKind`, `PortSignature`,
//!   `ArtifactSummary`, `DescriptorSummary`.
//! - [`bundle`](crate::artifact::bundle) owns `BundleManifest`, package/executable/component/artifact
//!   records, `BundleSimulation`, `BundleScenarioSection`,
//!   `BundleProvenance` family, `digest_source_files`.
//! - [`document`](crate::artifact::document) owns `RobotDocument`, `ComponentDocument`, capability
//!   declarations, native target records, and the inert DTO closure
//!   referenced by `BundleManifest.document`. Authored-file *parsing*
//!   stays in `cargo-phoxal`.
//! - [`simulation`](crate::artifact::simulation) owns `SimulatorTerminalEvidence`, `NativeBodySample`,
//!   `ScenarioExecutionReport`, `ScenarioStepEvidence`,
//!   `ScenarioCaptureEvidence`. Process orchestration stays in
//!   `cargo-phoxal`.
//! - [`scenario`](mod@crate::artifact::scenario) owns `BundleScenarioProgram`, `BundleScenarioProducer`,
//!   scenario artifact-path constant. Bundle assembly orchestration
//!   stays in `cargo-phoxal`.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod bundle;
pub mod document;
mod record;
pub mod scenario;
pub mod simulation;

pub use record::{
    ArtifactSummary, DescriptorSummary, InputKind, InputRecord, OutputKind, OutputRecord, PortKind,
    PortSignature, RUNTIME_RECORD, RuntimeRecord,
};
