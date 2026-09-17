//! Shared serialized artifact format.
//!
//! This crate owns the inert data records, format constants, and pure
//! validation/digest functions that the framework uses to communicate
//! artifact and bundle shape across the compiler, simulator, supervisor,
//! and SDK boundaries.
//!
//! It must not discover a project, inspect native executable sections,
//! walk a directory, invoke Cargo, open a network connection, or start a
//! process. Anything with those concerns belongs to `phoxal-project` or
//! to the simulator/supervisor/SDK application that consumes the format.
//!
//! ## Submodules
//!
//! - [`artifact`] — `RuntimeRecord`, `InputRecord`, `OutputRecord`,
//!   `PortKind`, `InputKind`, `OutputKind`, `PortSignature`,
//!   `ArtifactSummary`, `DescriptorSummary`.
//! - [`bundle`] — `BundleManifest`, package/executable/component/artifact
//!   records, `BundleSimulation`, `BundleScenarioSection`,
//!   `BundleProvenance` family, `digest_source_files`.
//! - [`document`] — `RobotDocument`, `ComponentDocument`, capability
//!   declarations, native target records, and the inert DTO closure
//!   referenced by `BundleManifest.document`. Authored-file *parsing*
//!   stays in `phoxal-project`.
//! - [`simulation`] — `SimulatorTerminalEvidence`, `NativeBodySample`,
//!   `ScenarioExecutionReport`, `ScenarioStepEvidence`,
//!   `ScenarioCaptureEvidence`. Process orchestration stays in
//!   `phoxal-project`.
//! - [`scenario`] — `BundleScenarioProgram`, `BundleScenarioProducer`,
//!   scenario artifact-path constant. Bundle assembly orchestration
//!   stays in `phoxal-project`.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod artifact;
pub mod bundle;
pub mod document;
pub mod simulation;
pub mod scenario;
