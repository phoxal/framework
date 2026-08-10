//! The framework-owned wire-contract catalogue: robot-domain payloads and
//! their semantic endpoints, grouped into families.
//!
//! Payload structs, enums, implementations, and tests are ordinary Rust items
//! in family-first modules. A sibling [`phoxal_api_fragment!`] declares only
//! that module's endpoints. Payloads own serde shape, construction invariants,
//! and domain behavior; they do not know their topic or delivery policy.
//! [`phoxal_api_tree!`] materializes deterministic descriptors and typed topic
//! builders, then re-exports the authored payloads through each family.
//!
//! A **family** is the first path segment of every fragment and the leading
//! segment of every key it declares: it names a semantic contract namespace,
//! not a revision. [`robot`] holds the contracts a robot participant authors
//! against, and `phoxal::api` re-exports it. Compatibility is owned entirely by
//! the framework train version each participant binary embeds, so no key,
//! descriptor, or body carries a per-API version.
//!
//! Endpoint semantics are fixed by the declaration: `State`, `Sample`, `Event`,
//! `Stream`, `Setpoint`, or bounded query. Source identity, robot/capture time,
//! ordered positions, loss, gaps, and terminal evidence remain bus metadata and
//! never become generated fields in a domain payload.

mod api;
pub use api::generated::*;
pub use phoxal_macros::{phoxal_api_fragment, phoxal_api_fragment_group, phoxal_api_tree};
