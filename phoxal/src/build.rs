//! Authoring helper for Protobuf build scripts.
//!
//! [`api`] is the package-local build-script entry point for authored `api/`
//! Protobuf sources and exact prepared robot selections.
//! The older explicit compilation functions remain for packages still using
//! dependency descriptors during the cutover.
//!
//! Owner manifests declare `phoxal` with the `build` feature in
//! `[build-dependencies]`; contract authoring does not
//! name `phoxal-build` or `phoxal-port` directly. This module pulls in no
//! runtime/transport/session/supervisor/native simulation dependencies and
//! therefore stays out of the public library graph.
//!
//! Phoxal's own bootstrap (`phoxal/build.rs`) keeps a direct
//! `phoxal-build` dependency identified by reason: it cannot depend on its
//! own not-yet-built library to generate its own protocols.

pub use phoxal_build::{
    API_PROTO, BuildApiConfig, DependencyDescriptor, api, compile_contracts,
    compile_contracts_with_dependencies, compile_contracts_with_dependencies_and_output,
    compile_contracts_with_output, compile_protos, compile_protos_with_dependencies,
    compile_protos_with_dependencies_and_output, compile_protos_with_output, descriptor_set_path,
    generate_contract_package, include_dir,
};

pub use phoxal_build::Error;
