//! The one sanctioned writer of the embedded participant-metadata document.
//!
//! [`ParticipantMetadata`](crate::metadata::ParticipantMetadata) is
//! deserialize-only, so the document's serialized shape is defined exactly
//! once, here, by [`ParticipantMetadataRecord`]. The framework train version is
//! the document's whole compatibility claim and arrives as a typed
//! [`FrameworkVersion`](crate::version::FrameworkVersion); no writer anywhere
//! invents a version of its own.
//!
//! A role macro cannot call `serde_json` though: the record it emits lands in a
//! `#[link_section]` static, whose length must be a constant, and its
//! `config_schema` is only known after `rustc` const-evaluates the recursive
//! `ParticipantConfig::SCHEMA_JSON` tree in the participant's own crate. So the
//! const-eval path is
//! [`participant_metadata_json!`](crate::participant_metadata_json), which
//! composes the same document from the same typed values through
//! `const_format`. The two are one writer in two evaluation modes, and
//! `the_const_writer_emits_exactly_what_the_typed_record_serializes` fails if
//! they ever disagree.

use serde::Serialize;

use crate::metadata::{
    ParticipantContract, ParticipantKind, ParticipantMetadata, ParticipantRequirement,
};
use crate::version::FrameworkVersion;
use crate::wire_schema::{DescribeWire, WireSchema};

/// `const_format::concatcp!`, made reachable as `$crate::emit::concatcp!`.
///
/// [`participant_metadata_json!`](crate::participant_metadata_json) expands
/// inside a participant's own crate, which does not depend on `const_format`,
/// so the macro cannot name that crate directly. Routing the call through this
/// crate is what makes the expansion hygienic: it resolves in the participant
/// crate no matter what is in scope there. That obligation is the only reason
/// this is public, and it is why the item cannot be made private or removed
/// while the macro exists.
#[doc(hidden)]
pub use const_format::concatcp;

/// The serialize side of the embedded metadata document.
///
/// The serialized form of one [`ParticipantContract`](crate::metadata::ParticipantContract)
/// while its artifact id is still a const string in a role-macro expansion.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipantContractRecord<'a> {
    pub framework: FrameworkVersion,
    pub id: &'a str,
    pub kind: ParticipantKind,
    pub requirement: Option<ParticipantRequirement>,
    pub config_schema: serde_json::Value,
}

/// Its variants and renames mirror
/// [`ParticipantMetadata`](crate::metadata::ParticipantMetadata) exactly - that
/// is the point: a record written through this type is, by construction, a
/// document the parser accepts.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "schema")]
pub enum ParticipantMetadataRecord<'a> {
    #[serde(rename = "phoxal/participant-metadata/v0")]
    V0 {
        #[serde(flatten)]
        contract: ParticipantContractRecord<'a>,
    },
}

impl DescribeWire for ParticipantContractRecord<'_> {
    // Invariant: this record is the serialize side of one contract, so it
    // declares that contract's shape rather than a second copy of it. A field
    // added to only one of the two stops matching here.
    fn wire_schema() -> WireSchema {
        ParticipantContract::wire_schema()
    }
}

impl DescribeWire for ParticipantMetadataRecord<'_> {
    // Invariant: the writer and the parser are one document, so the shape is
    // stated exactly once, on the parser.
    fn wire_schema() -> WireSchema {
        ParticipantMetadata::wire_schema()
    }
}

/// Const-evaluates the embedded metadata document.
///
/// `framework` is the canonical spelling of the framework train version,
/// spliced from the facade constant that owns it. `id` is the participant
/// identity literal and `config_schema` is a `&'static str` holding
/// already-composed JSON.
///
/// Hidden from the docs: this is the ABI writer the role macros expand into,
/// not a surface a participant author calls. It is `#[macro_export]` only
/// because macro expansion in another crate needs it to be nameable.
#[doc(hidden)]
#[macro_export]
macro_rules! participant_metadata_json {
    (
        framework = $framework:expr,
        id = $id:expr,
        kind = $kind:expr,
        requirement = $requirement:expr,
        config_schema = $config_schema:expr $(,)?
    ) => {{
        // `concatcp!` takes constants, not method calls, so each value resolves
        // to its canonical spelling one step earlier.
        const __PHOXAL_FRAMEWORK: &str = $framework;
        const __PHOXAL_KIND: &str = $kind.as_str();
        const __PHOXAL_REQUIREMENT: &str = match $requirement {
            Some(requirement) => match requirement {
                $crate::metadata::ParticipantRequirement::DifferentialDriveVelocity => {
                    "\"differential_drive_velocity\""
                }
            },
            None => "null",
        };

        $crate::emit::concatcp!(
            "{\"schema\":\"phoxal/participant-metadata/v0\",\"framework\":\"",
            __PHOXAL_FRAMEWORK,
            "\",\"id\":\"",
            $id,
            "\",\"kind\":\"",
            __PHOXAL_KIND,
            "\",\"requirement\":",
            __PHOXAL_REQUIREMENT,
            ",\"config_schema\":",
            $config_schema,
            "}"
        )
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::ParticipantMetadata;

    const CONFIG_SCHEMA: &str = r#"{"type":"null"}"#;

    const EMBEDDED: &str = participant_metadata_json!(
        framework = FrameworkVersion::CURRENT_SPELLING,
        id = "drive",
        kind = ParticipantKind::Service,
        requirement = None,
        config_schema = CONFIG_SCHEMA,
    );

    fn typed_record() -> ParticipantMetadataRecord<'static> {
        ParticipantMetadataRecord::V0 {
            contract: ParticipantContractRecord {
                framework: FrameworkVersion::CURRENT,
                id: "drive",
                kind: ParticipantKind::Service,
                requirement: None,
                config_schema: serde_json::json!({"type": "null"}),
            },
        }
    }

    #[test]
    fn the_const_writer_emits_exactly_what_the_typed_record_serializes() {
        let const_written: serde_json::Value =
            serde_json::from_str(EMBEDDED).expect("the const writer emits a JSON document");
        let typed = serde_json::to_value(typed_record()).expect("the typed record serializes");
        assert_eq!(const_written, typed);
    }

    /// The bytes that actually land in the linker section are checked against
    /// the declared document shape, so the const evaluation mode is covered by
    /// the same declaration the typed one is.
    #[test]
    fn the_const_written_bytes_have_the_declared_document_shape() {
        let const_written: serde_json::Value =
            serde_json::from_str(EMBEDDED).expect("the const writer emits a JSON document");
        assert_eq!(
            ParticipantMetadataRecord::wire_schema().conforms(&const_written),
            Ok(())
        );
    }

    #[test]
    fn an_emitted_record_parses_back_into_the_typed_contract() {
        let metadata = ParticipantMetadata::from_bytes(EMBEDDED.as_bytes())
            .expect("the writer's own output must satisfy the parser");
        let contract = metadata.contract();

        assert_eq!(contract.framework, FrameworkVersion::CURRENT);
        assert_eq!(contract.id.as_str(), "drive");
        assert_eq!(contract.kind, ParticipantKind::Service);
        assert_eq!(contract.requirement, None);
        assert_eq!(contract.config_schema, serde_json::json!({"type": "null"}));
    }

    /// The embedded section is read as a whole document, so the const writer
    /// has to emit one - not a fragment a reader would have to repair.
    #[test]
    fn the_const_written_document_is_self_contained() {
        assert!(
            EMBEDDED.starts_with('{') && EMBEDDED.ends_with('}'),
            "{EMBEDDED}"
        );
        assert_eq!(EMBEDDED.len(), EMBEDDED.trim().len());
    }
}
