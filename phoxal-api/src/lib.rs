//! Versioned robot-domain payloads and their semantic endpoints.
//!
//! Payload structs and enums are ordinary Rust types in version/domain modules.
//! They own serde shape, construction invariants, and domain behavior; they do
//! not know their topic or delivery policy. The [`phoxal_api!`] declaration maps
//! those payloads onto generated endpoint descriptors and deterministic typed
//! topic builders.
//!
//! Normal robot participants use the train-selected [`latest`] facade (reexported
//! as `phoxal::api`). External compatibility clients may deliberately select a
//! maintained concrete revision such as [`v0_1`] or [`v0_2`]. Every materialized
//! revision is complete and independent at runtime: inheritance exists only in
//! macro authoring.
//!
//! Endpoint semantics are fixed by the declaration: `State`, `Sample`, `Event`,
//! `Stream`, `Setpoint`, or bounded query. Source identity, robot/capture time,
//! ordered positions, loss, gaps, and terminal evidence remain bus metadata and
//! never become generated fields in a domain payload.
//!
//! This crate contains robot contracts only. Runtime/simulation infrastructure
//! and supervisor process protocols have separate owners.

mod api;
pub use api::*;
pub use phoxal_macros::{phoxal_api, phoxal_api_tree, phoxal_protocol};

pub mod domains;

#[cfg(test)]
mod tests;
