//! Public dependency surface for the framework-owned supervisor package.
//!
//! The package is selected as a normal Cargo dependency so its executable and
//! public session identity resolve through one graph.  Runtime behavior stays
//! in the `phoxal` supervisor profile and the binary remains the only launched
//! target.

/// Public deployment identity accepted by the supervisor process and clients.
pub use phoxal::communication::DeploymentTarget;

/// Supervisor package contract discriminator.
pub const SUPERVISOR_SCHEMA: &str = "phoxal/supervisor/v0";
