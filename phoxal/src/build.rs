//! Authoring helper for Protobuf build scripts.
//!
//! [`api`] is the package-local build-script entry point: one authored
//! service declaration (`service.yaml`, a component's embedded
//! `component.yaml` sections, or a robot project's `robot.yaml` brain
//! section) is the sole endpoint authority, and Protobuf files carry message
//! definitions only.
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

pub use phoxal_build::{BuildApiConfig, Error, api};
