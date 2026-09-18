//! Test aggregator for the project module.
//!
//! These tests live inside the `project` module so they share the same
//! dependency closure as the implementation they exercise. Each submodule
//! corresponds to a former top-level integration test of the now-collapsed
//! `phoxal-project` crate; their internal `use phoxal_project::...` paths
//! have been rewritten to `use crate::project::...` against the new owner.

mod project;
mod scenario_prep;
mod source_digest;
