//! Deterministic public-API tests that need an external crate's view.
//!
//! One executable with modules inside: separate test targets each cost
//! their own compilation for no isolation benefit here.
//! `runtime_behavior_observers` keeps its own binary because tracing
//! delivery consults a process-wide call-site interest cache; the
//! compiler contracts live in the `compile` suite.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

mod config_derive;
mod runtime_authoring;
mod runtime_semantics;

mod path_attachments;
