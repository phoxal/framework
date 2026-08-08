//! The read side of the embedded participant-metadata document.
//!
//! Every participant binary carries one [`ParticipantContract`] in a linker
//! section. The same contract is persisted with the reusable artifact in a
//! runtime bundle; keeping one type for both boundaries prevents a binary's
//! identity and compatibility claims from being copied into a second DTO.

use serde::{Deserialize, Serialize};

use crate::identity::ParticipantArtifactId;
use crate::version::{BusAbi, LaunchAbi, RobotApi, RuntimeSchema};

/// Every process-boundary version identity one participant binary speaks.
/// Authored source grammars are intentionally absent: a runtime process
/// consumes the compiled runtime document, not `robot.yaml`, component files,
/// or simulation source.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipantSchemas {
    /// The bus wire ABI.
    pub bus: BusAbi,
    /// The process launch compatibility identity.
    pub launch: LaunchAbi,
    /// The compiled runtime document grammar.
    pub runtime: RuntimeSchema,
}

/// The complete compatibility contract embedded in one reusable participant
/// artifact.
///
/// This is the single contract value shared by binary metadata and the
/// persisted runtime bundle. In particular, it does not contain a launched
/// instance id: one artifact may serve many runtime participant instances.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipantContract {
    /// The compile-time identity of the reusable artifact.
    pub id: ParticipantArtifactId,
    /// The role kind declared by the artifact's role macro.
    pub kind: ParticipantKind,
    /// The robot API revision used by the artifact.
    pub api: RobotApi,
    /// The process-boundary schemas used by the artifact.
    pub schemas: ParticipantSchemas,
    /// The optional static topology requirement.
    #[serde(default)]
    pub requirement: Option<ParticipantRequirement>,
    /// The exact JSON Schema emitted for the artifact's config type.
    pub config_schema: serde_json::Value,
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
/// understands: malformed JSON, an unknown schema tag, an unknown version
/// identity, or an unknown field.
#[derive(Debug, thiserror::Error)]
#[error("participant metadata is not a readable phoxal document: {0}")]
pub struct MetadataError(#[from] serde_json::Error);

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMAS: &str = r#"{"bus":"phoxal/bus-abi/v0","launch":"phoxal/participant-launch/v0","runtime":"phoxal/runtime/v0"}"#;

    fn record(fields: &str) -> Vec<u8> {
        format!(
            r#"{{"schema":"phoxal/participant-metadata/v0","api":"phoxal/robot-api/v0.2","schemas":{SCHEMAS},{fields}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn a_v0_record_parses_into_the_canonical_artifact_contract() {
        let ParticipantMetadata::V0 { contract } = ParticipantMetadata::from_bytes(&record(
            r#""id":"drive","kind":"service","config_schema":{"type":"null"}"#,
        ))
        .expect("the exact document a role macro embeds must parse");

        assert_eq!(contract.api, RobotApi::V0_2);
        assert_eq!(contract.schemas.bus, BusAbi::V0);
        assert_eq!(contract.schemas.launch, LaunchAbi::V0);
        assert_eq!(contract.schemas.runtime, RuntimeSchema::V0);
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

    #[test]
    fn an_unknown_schema_tag_is_rejected() {
        let bytes = br#"{"schema":"phoxal/participant-metadata/v1","api":"phoxal/robot-api/v0.1","schemas":{"bus":"phoxal/bus-abi/v0","launch":"phoxal/participant-launch/v0","runtime":"phoxal/runtime/v0"},"id":"drive","kind":"service","config_schema":null}"#;
        assert!(ParticipantMetadata::from_bytes(bytes).is_err());
    }

    #[test]
    fn a_version_identity_from_another_train_is_rejected_by_name() {
        let bytes = format!(
            r#"{{"schema":"phoxal/participant-metadata/v0","api":"phoxal/robot-api/v0.3","schemas":{SCHEMAS},"id":"drive","kind":"service","config_schema":null}}"#
        )
        .into_bytes();
        let message = ParticipantMetadata::from_bytes(&bytes)
            .expect_err("an API revision this train does not speak must not parse")
            .to_string();
        assert!(message.contains("phoxal/robot-api/v0.3"), "{message}");
        assert!(message.contains("phoxal/robot-api/v0.2"), "{message}");
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
    fn a_record_missing_a_runtime_schema_is_rejected() {
        let bytes = br#"{"schema":"phoxal/participant-metadata/v0","api":"phoxal/robot-api/v0.1","schemas":{"bus":"phoxal/bus-abi/v0","launch":"phoxal/participant-launch/v0"},"id":"drive","kind":"service","config_schema":null}"#;
        assert!(ParticipantMetadata::from_bytes(bytes).is_err());
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

    #[test]
    fn a_valid_record_without_requirement_remains_readable() {
        let metadata = ParticipantMetadata::from_bytes(&record(
            r#""id":"drive","kind":"service","config_schema":null"#,
        ))
        .expect("metadata without an optional requirement parses");
        let contract = metadata.contract();
        assert_eq!(contract.requirement, None);
    }
}
