//! Stable data crossing Phoxal process boundaries.
//!
//! This crate deliberately contains no participant runner, bus transport,
//! command-line parser, or project compiler. It is the shared vocabulary used
//! by framework participants, the framework supervisor, and clients such as
//! `phoxal-cli`.
//!
//! There is no crate-root facade: every public item is reached through the
//! module that owns its contract, so a type has exactly one path and an import
//! already says which process-boundary contract it belongs to.
//!
//! - [`identity`] - the execution, producer, and timeline identities that reach
//!   the wire.
//! - [`version`] - the version identities two binaries compare to establish
//!   that they speak the same contracts.
//! - [`metadata`] - the record every participant binary embeds at compile time,
//!   and its strict parser.
//! - [`emit`] - the one sanctioned writer of that record, in both of its
//!   evaluation modes.
//! - [`rendezvous`] - the shared host paths and advisory locking through which
//!   a client and the one execution supervisor find and fence each other.
//! - [`wire_schema`] - the deterministic model of the shapes those contracts
//!   put on the wire, which compatibility CI checks against published
//!   baselines.
//! - [`contract_surface`] - the record model each contract-owning crate states
//!   its whole boundary in, built out of those shapes.

pub mod clock;
pub mod contract_surface;
pub mod emit;
pub mod identity;
pub mod metadata;
pub mod rendezvous;
pub mod version;
pub mod wire_schema;

/// The contract surface this crate owns: the participant-metadata document
/// every binary embeds.
///
/// Not public API. It exists so compatibility CI can read a crate's declared
/// process boundary out of the crate itself, and its shape may change with the
/// checker.
#[doc(hidden)]
pub mod __compat {
    use crate::contract_surface::{ContractRecord, ContractSurface};
    use crate::metadata::ParticipantMetadata;
    use crate::wire_schema::DescribeWire;

    /// The canonical rendering of this crate's contract surface.
    #[must_use]
    pub fn contract_surface() -> String {
        ContractSurface::new([ContractRecord::document(
            "ParticipantMetadata",
            crate::metadata::PARTICIPANT_METADATA_SCHEMA_TAG,
            ParticipantMetadata::wire_schema(),
        )])
        .canonical_json()
    }

    #[cfg(test)]
    mod tests {
        use super::contract_surface;

        /// The surface is one JSON document, it names the embedded document's
        /// tag, and two calls produce the same bytes - which is what lets a
        /// checker compare it with a stored baseline by string equality.
        #[test]
        fn the_surface_is_deterministic_json_naming_the_metadata_document() {
            let rendered = contract_surface();
            serde_json::from_str::<serde_json::Value>(&rendered).expect("the surface is JSON");
            assert_eq!(contract_surface(), rendered);
            assert!(
                rendered.contains(crate::metadata::PARTICIPANT_METADATA_SCHEMA_TAG),
                "{rendered}"
            );
            assert!(rendered.contains(r#""record":"document""#), "{rendered}");
            assert!(rendered.contains("config_schema"), "{rendered}");
        }
    }
}
