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
//! `tools/cargo-phoxal/project/src/preparation.rs`; the actual
//! `cargo metadata` / `cargo test --no-run` invocation lives in
//! `tools/cargo-phoxal/project/src/scenario/build.rs` and `run.rs`
//! (P4).

mod bundle;
pub mod case_host;
mod discovery;
mod harness;
mod run;

pub use bundle::{SCENARIO_NONDEPLOYABLE, ScenarioBundle, SubstitutedEdge};
pub use discovery::{DiscoveredScenario, DiscoveryError, discover_scenarios, module_identifier};
pub use harness::generate_harness_source;
pub use run::{ScenarioListEntry, ScenarioRunError, list_scenarios, run_scenario};
