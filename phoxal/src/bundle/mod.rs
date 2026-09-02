//! The persisted bundle boundary.
//!
//! `crate::authoring` compiles authored YAML/URDF into a canonical robot; this
//! module owns the artifact that remains after that source tree is gone.
//!
//! ```text
//! <bundle>/
//! ├── manifest.json
//! ├── assets/
//! └── bin/
//! ```
//!
//! That is the whole layout. There is no index, no digest table and no
//! participant list: the expected process set is `brain` plus every key of the
//! manifest's `services` and `components`, which anyone holding the manifest can
//! derive, and every participant reads its own configuration out of the same
//! document. A binary is found in `bin/` by the id it was launched under.
//!
//! [`RuntimeBundle::open`] parses the manifest and does nothing else. The
//! supervisor owns that local reader, while launched participants receive the
//! manifest through `supervisor/info` and lazily materialize assets from its
//! bounded reader. [`ParticipantAssets::read`] reads the cached file below
//! `assets/` by its [`AssetId`](crate::model::AssetId); that id is already a
//! validated relative path and [`BundlePath`] validates the join again, so a
//! read cannot leave `assets/`.
//!
//! [`BundleWriter::write`] takes the manifest, the assets, and a map from
//! bundle-relative destination to the executable to copy there. It assembles the
//! bundle in a private sibling directory and renames it onto its final name, so
//! the target is either absent or a complete bundle; an existing target is
//! refused, and a failed write removes its own staging directory.
//!
//! Nothing here verifies anything beyond "the manifest parses". Integrity lives
//! in the archive: `phoxal build` writes `build.phoxal` alongside its
//! `build.phoxal.sha256`, and `phoxal install` refuses a mismatch. Once a bundle
//! is on disk, the supervisor and every participant trust what is there - a
//! second fence inside the bundle would only re-check bytes nobody re-signed.

mod path;
pub use path::{BundlePath, BundlePathError};
mod asset;
pub use asset::ParticipantAssets;
mod reader;
pub use reader::RuntimeBundle;
mod error;
pub use error::BundleError;
mod writer;
pub use writer::BundleWriter;
mod fs;
pub(crate) use fs::{
    BundleRoot, copy_executable_source, create_staging_root, ensure_staging_directory,
    open_bundle_file, prepare_publish_parent, publish_staging_root, read_manifest_document,
    reject_existing_target, write_new_file,
};

/// The persisted document filename at the bundle root.
pub const MANIFEST_FILE: &str = "manifest.json";
/// The participant-readable asset directory.
pub const ASSETS_DIR: &str = "assets";
/// The launchable binary directory.
pub const BIN_DIR: &str = "bin";

#[cfg(test)]
mod bundle_boundary_tests;

/// The contract surface this crate owns: the one persisted document.
///
/// Not public API. It exists so compatibility CI can read this crate's declared
/// process boundary out of the crate itself.
///
/// The document body reaches all the way down: the canonical robot it embeds,
/// that robot's services, components, capabilities and structure are all part of
/// the one record below, because a process reading `manifest.json` has to agree
/// with the writer about every one of them.
#[doc(hidden)]
pub mod __compat {
    use crate::__compat::surface::{ContractRecord, ContractSurface};
    use crate::__compat::wire::DescribeWire;
    use crate::model::manifest::{MANIFEST_SCHEMA, ManifestDocument};

    /// The canonical rendering of this module's own contract surface.
    #[must_use]
    pub fn contract_surface() -> String {
        let mut records = Vec::new();
        contract_records(&mut records);
        ContractSurface::new(records).canonical_json()
    }

    /// This module's records, for the crate aggregate.
    pub(crate) fn contract_records(out: &mut Vec<ContractRecord>) {
        out.push(ContractRecord::document(
            "ManifestDocument",
            MANIFEST_SCHEMA,
            ManifestDocument::wire_schema(),
        ));
    }

    #[cfg(test)]
    mod tests {
        use super::contract_surface;

        /// The surface is one deterministic JSON document that names the
        /// manifest's schema tag and reaches into the model the document
        /// embeds, so an accidentally shallow or empty surface cannot pass.
        #[test]
        fn the_surface_names_the_manifest_document_and_the_model_it_embeds() {
            let rendered = contract_surface();
            serde_json::from_str::<serde_json::Value>(&rendered).expect("the surface is JSON");
            assert_eq!(contract_surface(), rendered);
            for expected in [
                r#""tag":"phoxal/manifest/v0""#,
                r#""name":"ManifestDocument""#,
                // The document body, one field from each layer it embeds.
                r#""name":"services""#,
                r#""name":"components""#,
                r#""name":"component_types""#,
                r#""name":"Robot""#,
                r#""name":"Structure""#,
            ] {
                assert!(
                    rendered.contains(expected),
                    "{expected} missing: {rendered}"
                );
            }
        }
    }
}
