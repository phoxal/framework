//! Scenario tooling bridge.
//!
//! The [`discovery`] module scans `<robot-root>/scenarios/` for two file
//! layouts:
//!
//! - `scenarios/<name>.rs`
//! - `scenarios/<name>/mod.rs`
//!
//! Public scenario identity is the struct name (`scenarios/<StructIdent>`,
//! per the plan), so this module's job is to turn those paths into the
//! generated harness `#[path = "..."] mod ...;` declarations.
//!
//! The auto Cargo integration (test target + dev-dep) lives in
//! `phoxal/cargo/src/project/preparation.rs`; the actual
//! `cargo metadata` / `cargo test --no-run` invocation lives in `run.rs`.

pub mod case_host;
mod discovery;
mod harness;
mod run;

pub use discovery::{DiscoveredScenario, discover_scenarios};
pub use harness::generate_harness_source;
pub use run::{ScenarioRunError, list_scenarios, run_scenario};
