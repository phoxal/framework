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
//!   `MethodShape`, `InputDelivery`, `MethodSignature`,
//!   `ArtifactSummary`, `DescriptorSummary`.
//! - [`bundle`](crate::artifact::bundle) owns `BundleManifest`, package/executable/component/artifact
//!   records, model asset paths, and `BundleSimulation`.
//! - [`document`](crate::artifact::document) owns `RobotDocument`, `ComponentDocument`, capability
//!   declarations and native target records. Authored-file *parsing*
//!   stays in `cargo-phoxal`.
//! - [`simulation`](crate::artifact::simulation) owns `SimulatorTerminalEvidence`, `NativeBodySample`,
//!   `ScenarioExecutionReport`, `ScenarioStepEvidence`,
//!   `ScenarioCaptureEvidence`. Process orchestration stays in
//!   `cargo-phoxal`.
//! - [`simulation_run`](crate::artifact::simulation_run) owns the experiment
//!   specification that references an immutable bundle. Run hosting stays in
//!   `cargo-phoxal`.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod application;
pub mod bundle;
pub mod document;
mod record;
pub mod simulation;
pub mod simulation_run;

pub use crate::contracts::MethodShape;

pub use record::{
    ArtifactSummary, ConversionRoute, DescriptorSummary, InputDelivery, InputRecord,
    MethodSignature, OutputRecord, RUNTIME_RECORD, RuntimeRecord,
};

/// Native-free simulator installation command/status contract.
pub mod installation;
