//! The persisted runtime bundle boundary.
//!
//! `phoxal-manifest` compiles authored YAML/URDF into canonical model facts;
//! this crate owns the artifact that remains after that source tree is gone.
//! A runtime process reads only `runtime.json`, the indexed files below
//! `assets/`, and the selected binary below `bin/`. It never invokes the source
//! compiler and never discovers a participant from a catalog.
//!
//! ```text
//! <bundle>/
//! ├── runtime.json
//! ├── assets/
//! └── bin/
//! ```

pub use phoxal_runtime_contract::identity::ParticipantArtifactId;

mod path;
pub use path::{BundlePath, BundlePathError, DigestError, Sha256Digest};
mod asset;
pub use asset::{AssetIndex, AssetRecord, ParticipantAssets};
mod reader;
pub use reader::{ParticipantBundle, ParticipantRuntimeInputs, RuntimeBundle};
mod error;
pub use error::{BundleError, DocumentError, SelectionError};
mod writer;
pub use writer::BundleWriter;
mod fs;
pub(crate) use fs::{
    BundleRoot, copy_executable_source, create_staging_root, ensure_staging_directory,
    mark_staging_root_ready, open_bundle_file, open_executable_source, prepare_publish_parent,
    publish_staging_root, read_and_verify, read_runtime_document, reject_existing_target,
    require_layout_directories, validate_layout, write_new_file,
};
mod artifact;
pub use artifact::{BinaryReference, BinarySource};
mod participant;
pub use participant::RuntimeParticipant;
mod document;
pub use document::{ParticipantClock, Runtime, RuntimeDocument, RuntimeRouterConfig};

/// The only schema tag currently readable by this framework train.
pub const RUNTIME_SCHEMA: &str = "phoxal/runtime-bundle/v0";
/// The persisted document filename at the bundle root.
pub const RUNTIME_FILE: &str = "runtime.json";
/// The participant-readable asset directory.
pub const ASSETS_DIR: &str = "assets";
/// The supervisor-only binary directory.
pub const BIN_DIR: &str = "bin";
pub use phoxal_runtime_contract::metadata::MAX_RUNTIME_PARTICIPANTS;
#[cfg(test)]
mod bundle_boundary_tests;
