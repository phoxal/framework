//! Build-time generation from compiled Rust contract products.
//!
//! [`api`] reads local prepared participant products and component capability
//! declarations. All wire declarations are Rust-authored and use the SDK
//! schema records; independent protoc references belong to test fixtures.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

use std::path::PathBuf;

mod api;
mod conversions;
mod manifest;
mod prepared;
mod schema_impls;
mod typed;

pub use api::validate_project_api;
pub use api::{BuildApiConfig, api};

pub use prepared::{
    CONTRACT_FILE, CONTRACT_GENERATION, DESCRIPTORS_FILE, PREPARED_ROOT, PreparedContract,
    PreparedContractFile, PreparedExecutable, PreparedSelection, prepared_dir, prepared_input_root,
    prepared_instance_dir, prepared_key, read_descriptor_bytes, read_prepared, read_prepared_for,
    read_prepared_instance, validate_prepared_key, write_prepared, write_prepared_instance,
};

/// Rust path of a built-in payload identity in generated robot code.
#[must_use]
pub fn sdk_type_path(fqn: &str) -> Option<String> {
    if fqn == "google.protobuf.Empty" {
        return Some("::phoxal::contracts::Empty".to_owned());
    }
    schema_impls::sdk_owned_path(fqn).map(str::to_owned)
}

/// Encodes one descriptor set for pool decoding.
pub(crate) fn encode_file_descriptor_set(set: &prost_types::FileDescriptorSet) -> Vec<u8> {
    use prost::Message as _;
    set.encode_to_vec()
}

/// Errors reported while compiling or validating a service-owned contract.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A required Cargo build-script environment value is unavailable.
    #[error("Cargo build environment is missing {0}")]
    MissingEnvironment(&'static str),
    /// A source or include path cannot be resolved.
    #[error("cannot resolve Protobuf path {path}: {source}")]
    Path {
        /// The failing path.
        path: PathBuf,
        /// The filesystem error.
        source: std::io::Error,
    },
    /// A prepared descriptor closure is invalid.
    #[error("invalid Protobuf descriptor closure: {0}")]
    Descriptor(#[from] prost_reflect::DescriptorError),
    /// An authored API selection or prepared source tree is invalid.
    #[error("API input {path}: {message}")]
    ApiInput { path: PathBuf, message: String },
    /// A robot document cannot be decoded for build-script generation.
    #[error("cannot parse robot document {path}: {source}")]
    ApiDocument {
        path: PathBuf,
        source: serde_yaml::Error,
    },
}
