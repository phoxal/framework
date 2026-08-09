//! Versioned robot-domain payloads and their semantic endpoints.
//!
//! Payload structs, enums, implementations, and tests are ordinary Rust items
//! in version-first modules. A sibling [`phoxal_api_fragment!`] declares only
//! that module's endpoints. Payloads own serde shape, construction invariants,
//! and domain behavior; they do not know their topic or delivery policy.
//! [`phoxal_api_tree!`] materializes deterministic descriptors and typed topic
//! builders, then re-exports the authored payloads through each revision.
//!
//! Normal robot participants use the train-selected [`latest`] facade
//! (reexported as `phoxal::api`). This pre-v1 train currently has one maintained
//! concrete revision, [`v0_1`]. Every materialized revision is complete and
//! independent at runtime: inheritance exists only in macro authoring.
//!
//! Endpoint semantics are fixed by the declaration: `State`, `Sample`, `Event`,
//! `Stream`, `Setpoint`, or bounded query. Source identity, robot/capture time,
//! ordered positions, loss, gaps, and terminal evidence remain bus metadata and
//! never become generated fields in a domain payload.
//!
//! This crate contains robot contracts only. Runtime/simulation infrastructure
//! and supervisor process protocols have separate owners.

mod api;
pub use api::generated::*;
pub use phoxal_macros::{phoxal_api_fragment, phoxal_api_fragment_group, phoxal_api_tree};
