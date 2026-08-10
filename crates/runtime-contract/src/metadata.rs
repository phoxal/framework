//! The read side of the embedded participant-metadata document.
//!
//! Every participant binary carries one [`ParticipantContract`] in a linker
//! section. The document states the framework train the binary was built from -
//! the one compatibility identity two Phoxal processes compare, for exact
//! equality - plus the participant's own facts: what it is, what it requires,
//! and the config it accepts. The same contract is persisted with the reusable
//! artifact in a runtime bundle; keeping one type for both boundaries prevents
//! a binary's identity and compatibility claims from being copied into a second
//! DTO.
//!
//! The document's own `schema` tag is a format discriminator, not a negotiated
//! identity: a reader refuses a tag it does not implement before it reads a
//! field.

use serde::{Deserialize, Serialize};

use crate::identity::ParticipantArtifactId;
use crate::version::FrameworkVersion;
use crate::wire_schema::{
    DescribeWire, EnumRepresentation, FieldPresence, VariantBody, WireField, WireSchema,
    WireVariant,
};

/// Maximum participant instances in one compiled runtime execution.
pub const MAX_RUNTIME_PARTICIPANTS: usize = 64;

/// The format tag of the embedded participant-metadata document.
///
/// A serde attribute cannot name a constant, so the spelling below is written
/// twice: once on [`ParticipantMetadata`]'s `rename` and once here. The
/// declared shape and the crate's contract surface both read it from here, and
/// `the_declared_document_shape_is_the_shape_the_writer_emits` serializes a
/// real record against that shape, so a drift between the two spellings fails a
/// test rather than shipping.
pub const PARTICIPANT_METADATA_SCHEMA_TAG: &str = "phoxal/participant-metadata/v0";

/// The complete compatibility contract embedded in one reusable participant
/// artifact.
///
/// This is the single contract value shared by binary metadata and the
/// persisted runtime bundle. In particular, it does not contain a launched
/// instance id: one artifact may serve many runtime participant instances.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipantContract {
    /// The framework train the artifact was built from, and the whole of what
    /// it claims about compatibility.
    pub framework: FrameworkVersion,
    /// The compile-time identity of the reusable artifact.
    pub id: ParticipantArtifactId,
    /// The role kind declared by the artifact's role macro.
    pub kind: ParticipantKind,
    /// The optional static topology requirement.
    pub requirement: Option<ParticipantRequirement>,
    /// The exact JSON Schema emitted for the artifact's config type.
    pub config_schema: serde_json::Value,
}

// The wire shapes in this module are hand-written rather than derived: the
// process-contract floor sits below `phoxal-macros` in the crate graph, so it
// cannot use the derive that reads these same serde attributes. Each
// implementation states the shape its adjacent `#[derive(Serialize)]` writes,
// and `the_declared_document_shape_is_the_shape_the_writer_emits` checks the
// two against each other rather than trusting either.
impl DescribeWire for ParticipantContract {
    // Invariant: this states what the derived `Serialize` above writes - one
    // map of the five declared field names, with `requirement` decodable while
    // absent because it is an `Option`.
    fn wire_schema() -> WireSchema {
        WireSchema::structure([
            WireField::required("framework", FrameworkVersion::wire_schema()),
            WireField::required("id", ParticipantArtifactId::wire_schema()),
            WireField::required("kind", ParticipantKind::wire_schema()),
            WireField::new(
                "requirement",
                WireSchema::option(ParticipantRequirement::wire_schema()),
                FieldPresence::Defaulted,
            ),
            WireField::required("config_schema", serde_json::Value::wire_schema()),
        ])
    }
}

/// What a participant binary is, as declared by the role macro it was built
/// with. A supervisor schedules and supervises a process by this alone; there
/// is no second, finer classification anywhere in the process contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantKind {
    Service,
    Driver,
    Simulator,
    /// The one mandatory root brain: the robot project's composition root,
    /// built from the root Cargo package and staged as `bin/brain`.
    Brain,
}

impl ParticipantKind {
    /// The wire token for this kind, identical to the `snake_case` rename
    /// serde derives. Const so the role macro can splice it into the embedded
    /// document during const-eval.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ParticipantKind::Service => "service",
            ParticipantKind::Driver => "driver",
            ParticipantKind::Simulator => "simulator",
            ParticipantKind::Brain => "brain",
        }
    }

    /// Every kind, so the wire declaration and `as_str` cannot cover different
    /// sets.
    const ALL: [Self; 4] = [Self::Service, Self::Driver, Self::Simulator, Self::Brain];
}

impl DescribeWire for ParticipantKind {
    // Invariant: this states what the derived `Serialize` above writes - one
    // externally tagged unit variant per kind, spelled by the `snake_case`
    // rename that `as_str` also returns.
    fn wire_schema() -> WireSchema {
        WireSchema::enumeration(
            EnumRepresentation::ExternallyTagged,
            ParticipantKind::ALL.map(|kind| WireVariant::new(kind.as_str(), VariantBody::Unit)),
        )
    }
}

/// The one topology requirement a participant binary may currently declare.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantRequirement {
    /// The stock `drive` service's topology and motor-command contract.
    DifferentialDriveVelocity,
}

impl ParticipantRequirement {
    /// The canonical wire token, identical to the serde rename.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DifferentialDriveVelocity => "differential_drive_velocity",
        }
    }

    /// Every requirement, so the wire declaration and `as_str` cannot cover
    /// different sets.
    const ALL: [Self; 1] = [Self::DifferentialDriveVelocity];
}

impl DescribeWire for ParticipantRequirement {
    // Invariant: this states what the derived `Serialize` above writes - one
    // externally tagged unit variant, spelled by the `snake_case` rename that
    // `as_str` also returns.
    fn wire_schema() -> WireSchema {
        WireSchema::enumeration(
            EnumRepresentation::ExternallyTagged,
            ParticipantRequirement::ALL
                .map(|requirement| WireVariant::new(requirement.as_str(), VariantBody::Unit)),
        )
    }
}

/// The record every participant binary embeds in its `.phoxal_meta` /
/// `__DATA,__phoxal_meta` section at compile time.
///
/// Deserialize-only on purpose: the sole writer is
/// [`crate::emit::ParticipantMetadataRecord`], so a reader can never
/// accidentally re-persist a document it merely parsed.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum ParticipantMetadata {
    #[serde(rename = "phoxal/participant-metadata/v0")]
    V0 {
        #[serde(flatten)]
        contract: ParticipantContract,
    },
}

impl DescribeWire for ParticipantMetadata {
    // Invariant: this states the one document both sides of this contract
    // handle - the parser above and `emit::ParticipantMetadataRecord`, which is
    // its only writer. The tag spelling comes from
    // [`PARTICIPANT_METADATA_SCHEMA_TAG`], which the serde attribute above
    // cannot name; every other part of the document composes from
    // `ParticipantContract`, since `#[serde(flatten)]` merges that contract's
    // fields into the tagged map and an internally tagged newtype variant over
    // it describes exactly the same result.
    fn wire_schema() -> WireSchema {
        WireSchema::enumeration(
            EnumRepresentation::InternallyTagged {
                tag: String::from("schema"),
            },
            [WireVariant::new(
                PARTICIPANT_METADATA_SCHEMA_TAG,
                VariantBody::newtype(ParticipantContract::wire_schema()),
            )],
        )
    }
}

impl ParticipantMetadata {
    /// Strictly parse the bytes of an embedded metadata section.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MetadataError> {
        serde_json::from_slice(bytes).map_err(MetadataError)
    }

    /// Borrow the canonical artifact contract carried by this record.
    #[must_use]
    pub const fn contract(&self) -> &ParticipantContract {
        match self {
            Self::V0 { contract } => contract,
        }
    }
}

/// An embedded metadata section that is not a document this framework train
/// understands: malformed JSON, an unknown schema tag, a malformed framework
/// version, or an unknown field.
#[derive(Debug, thiserror::Error)]
#[error("participant metadata is not a readable phoxal document: {0}")]
pub struct MetadataError(#[from] serde_json::Error);

#[cfg(test)]
mod tests {
    use super::*;

    fn record(fields: &str) -> Vec<u8> {
        format!(
            r#"{{"schema":"phoxal/participant-metadata/v0","framework":"0.57.2","requirement":null,{fields}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn a_v0_record_parses_into_the_canonical_artifact_contract() {
        let ParticipantMetadata::V0 { contract } = ParticipantMetadata::from_bytes(&record(
            r#""id":"drive","kind":"service","config_schema":{"type":"null"}"#,
        ))
        .expect("the exact document a role macro embeds must parse");

        assert_eq!(contract.framework, FrameworkVersion::new(0, 57, 2));
        assert_eq!(contract.id.as_str(), "drive");
        assert_eq!(contract.kind, ParticipantKind::Service);
        assert_eq!(contract.requirement, None);
        assert_eq!(contract.config_schema, serde_json::json!({"type": "null"}));
    }

    #[test]
    fn the_root_brain_kind_is_distinct_from_a_service() {
        let metadata = ParticipantMetadata::from_bytes(&record(
            r#""id":"brain","kind":"brain","config_schema":{"type":"null"}"#,
        ))
        .expect("the exact document `#[phoxal::brain]` embeds must parse");
        let contract = metadata.contract();
        assert_eq!(contract.id.as_str(), "brain");
        assert_eq!(contract.kind, ParticipantKind::Brain);
        assert_ne!(contract.kind, ParticipantKind::Service);
    }

    #[test]
    fn the_kind_wire_token_is_the_serde_rename() {
        for kind in [
            ParticipantKind::Service,
            ParticipantKind::Driver,
            ParticipantKind::Simulator,
            ParticipantKind::Brain,
        ] {
            let json = serde_json::to_string(&kind).expect("a unit variant serializes");
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
    }

    /// The `schema` tag is a format discriminator: a reader refuses a document
    /// grammar it does not implement before it reads a field.
    #[test]
    fn an_unknown_schema_tag_is_rejected() {
        let bytes = br#"{"schema":"phoxal/participant-metadata/v1","framework":"0.57.2","id":"drive","kind":"service","requirement":null,"config_schema":null}"#;
        assert!(ParticipantMetadata::from_bytes(bytes).is_err());
    }

    /// A record whose framework version is any spelling but the canonical
    /// SemVer string is not a document this train can read.
    #[test]
    fn a_non_canonical_framework_version_is_rejected() {
        for framework in ["\"v0.57.2\"", "\"0.57\"", "\"0.57.2-rc.1\"", "null"] {
            let bytes = format!(
                r#"{{"schema":"phoxal/participant-metadata/v0","framework":{framework},"id":"drive","kind":"service","requirement":null,"config_schema":null}}"#
            )
            .into_bytes();
            assert!(
                ParticipantMetadata::from_bytes(&bytes).is_err(),
                "framework {framework} must not parse"
            );
        }
    }

    #[test]
    fn an_unknown_field_is_rejected() {
        assert!(
            ParticipantMetadata::from_bytes(&record(
                r#""id":"drive","kind":"service","config_schema":null,"extra":true"#,
            ))
            .is_err()
        );
    }

    #[test]
    fn a_record_missing_its_framework_version_is_rejected() {
        let bytes = br#"{"schema":"phoxal/participant-metadata/v0","id":"drive","kind":"service","requirement":null,"config_schema":null}"#;
        assert!(ParticipantMetadata::from_bytes(bytes).is_err());
    }

    /// The declared document shape is checked against a real record rather
    /// than asserted, which is what keeps a hand-written declaration honest
    /// about the flattened contract it merges under the tag.
    #[test]
    fn the_declared_document_shape_is_the_shape_the_writer_emits() {
        let emitted = crate::emit::ParticipantMetadataRecord::V0 {
            contract: crate::emit::ParticipantContractRecord {
                framework: FrameworkVersion::CURRENT,
                id: "drive",
                kind: ParticipantKind::Service,
                requirement: Some(ParticipantRequirement::DifferentialDriveVelocity),
                config_schema: serde_json::json!({"type": "null"}),
            },
        };
        let json = serde_json::to_value(&emitted).expect("the writer's record serializes");
        assert_eq!(ParticipantMetadata::wire_schema().conforms(&json), Ok(()));

        // The reader's declaration and the writer's are one shape, because the
        // two types are one document in two evaluation modes.
        assert_eq!(
            ParticipantMetadata::wire_schema(),
            crate::emit::ParticipantMetadataRecord::wire_schema()
        );
    }

    /// The declared shape says an absent `requirement` still decodes, so the
    /// parser has to agree.
    #[test]
    fn an_absent_requirement_decodes_exactly_as_the_declared_shape_says() {
        let bytes = br#"{"schema":"phoxal/participant-metadata/v0","framework":"0.57.2","id":"drive","kind":"service","config_schema":null}"#;
        let metadata =
            ParticipantMetadata::from_bytes(bytes).expect("an absent optional field decodes");
        assert_eq!(metadata.contract().requirement, None);
    }

    #[test]
    fn requirement_tokens_round_trip() {
        let requirement = ParticipantRequirement::DifferentialDriveVelocity;
        let json = serde_json::to_string(&requirement).expect("requirement serializes");
        assert_eq!(json, format!("\"{}\"", requirement.as_str()));
        assert_eq!(
            serde_json::from_str::<ParticipantRequirement>(&json).expect("requirement parses"),
            requirement
        );
    }
}
