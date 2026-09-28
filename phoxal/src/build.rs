//! Local build helper for prepared Rust contract bindings.
//!
//! [`api`] reads compiled participant products prepared by `cargo phoxal`.
//! Component capabilities in `component.yaml` also derive their standard
//! endpoint bindings without separate endpoint sections.
//!
//! Owner manifests declare `phoxal` with the `build` feature in
//! `[build-dependencies]`; contract authoring does not name `phoxal-build`
//! directly. This module pulls in no runtime/transport/session/supervisor/
//! native simulation dependencies and therefore stays out of the public
//! library graph.
//!
//! Phoxal's own bootstrap (`phoxal/build.rs`) keeps a direct
//! `phoxal-build` dependency identified by reason: it cannot depend on its
//! own not-yet-built library to generate its own protocols.

pub use phoxal_build::{
    BuildApiConfig, Error, api, compile_protos, compile_protos_with_output, include_dir,
};
