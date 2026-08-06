//! Authoritative identifiers for the authored document grammars a framework
//! train speaks.
//!
//! `phoxal-manifest` is where these documents are actually parsed, and its
//! `#[serde(tag = "schema")]` variants carry the same tokens - a serde rename
//! takes a literal, so it cannot name a constant. These constants exist here,
//! on the process-boundary floor, because a participant binary's embedded
//! compatibility record has to declare them and the runtime facade cannot
//! depend on the manifest compiler. `workspace-policy` pins the two together
//! by round-tripping a real document against these values.

/// The authored robot document grammar (`robot.yaml`).
pub const ROBOT_DOCUMENT_SCHEMA: &str = "robot/v0";

/// The authored component document grammar (`component.yaml`).
pub const COMPONENT_DOCUMENT_SCHEMA: &str = "component/v0";

/// The authored simulation document grammar (`simulation.yaml`).
pub const SIMULATION_DOCUMENT_SCHEMA: &str = "simulation/v0";
