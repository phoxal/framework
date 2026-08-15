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
    open_bundle_file, open_executable_source, prepare_publish_parent, publish_staging_root,
    read_and_verify, read_runtime_document, reject_existing_target, require_layout_directories,
    validate_layout, write_new_file,
};
mod artifact;
pub use artifact::{BinaryReference, BinarySource};
mod participant;
pub use participant::RuntimeParticipant;
mod document;
pub use document::{ParticipantClock, Runtime, RuntimeDocument};

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

/// The contract surface this crate owns: the one persisted runtime document.
///
/// Not public API. It exists so compatibility CI can read this crate's declared
/// process boundary out of the crate itself.
///
/// The document body reaches all the way down: the canonical robot it embeds,
/// that robot's components, capabilities and structure, and the artifact
/// contracts staged beside it are all part of the one record below, because a
/// process reading `runtime.json` has to agree with the writer about every one
/// of them.
#[doc(hidden)]
pub mod __compat {
    use phoxal_runtime_contract::contract_surface::{ContractRecord, ContractSurface};
    use phoxal_runtime_contract::wire_schema::DescribeWire;

    use crate::{RUNTIME_SCHEMA, RuntimeDocument};

    /// The canonical rendering of this crate's contract surface.
    #[must_use]
    pub fn contract_surface() -> String {
        ContractSurface::new([ContractRecord::document(
            "RuntimeDocument",
            RUNTIME_SCHEMA,
            RuntimeDocument::wire_schema(),
        )])
        .canonical_json()
    }

    #[cfg(test)]
    mod tests {
        use super::contract_surface;

        /// The surface is one deterministic JSON document that names the
        /// bundle's schema tag and reaches into the model the document embeds,
        /// so an accidentally shallow or empty surface cannot pass.
        #[test]
        fn the_surface_names_the_runtime_document_and_the_model_it_embeds() {
            let rendered = contract_surface();
            serde_json::from_str::<serde_json::Value>(&rendered).expect("the surface is JSON");
            assert_eq!(contract_surface(), rendered);
            for expected in [
                r#""tag":"phoxal/runtime-bundle/v0""#,
                r#""name":"RuntimeDocument""#,
                // The document body, one field from each layer it embeds.
                r#""name":"participants""#,
                r#""name":"artifacts""#,
                r#""name":"config_schema""#,
                r#""name":"Robot""#,
                r#""name":"Structure""#,
                r#""name":"component_instances""#,
            ] {
                assert!(
                    rendered.contains(expected),
                    "{expected} missing: {rendered}"
                );
            }
        }
    }
}
