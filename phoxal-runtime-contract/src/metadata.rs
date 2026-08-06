use serde::{Deserialize, Serialize};

use crate::version::{BusAbi, ComponentSchema, LaunchAbi, RobotApi, RobotSchema, SimulationSchema};

/// Every version identity one participant binary speaks. This is the whole
/// compatibility surface between a `phoxal-cli` and a built participant: there
/// is no package-metadata table, no version file, and no framework-SemVer
/// floor anywhere in the process contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipantSchemas {
    /// The bus wire ABI.
    pub bus: BusAbi,
    /// The launch record / environment ABI.
    pub launch: LaunchAbi,
    /// The authored robot document grammar.
    pub robot: RobotSchema,
    /// The authored component document grammar.
    pub component: ComponentSchema,
    /// The authored simulation document grammar.
    pub simulation: SimulationSchema,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantKind {
    Service,
    Driver,
    Simulator,
    /// The one mandatory root brain: the robot project's composition root,
    /// built from the root Cargo package and staged as `bin/brain`. It is a
    /// checked, clocked graph participant with the ordinary typed-I/O surface
    /// and no privileged capability, and it is never an authored service.
    Brain,
}

impl ParticipantKind {
    /// The wire token for this kind, identical to the `snake_case` rename
    /// serde derives. Const so the role macro can splice it into the embedded
    /// document during const-eval; pinned to the rename by a test below.
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

/// The record every participant binary embeds in its `.phoxal_meta` /
/// `__DATA,__phoxal_meta` section at compile time.
///
/// Deserialize-only on purpose: the sole writer is
/// [`emit::ParticipantMetadataRecord`], so a reader can never accidentally
/// re-persist a document it merely parsed.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum ParticipantMetadata {
    #[serde(rename = "phoxal/participant-metadata/v0")]
    V0 {
        api: RobotApi,
        schemas: ParticipantSchemas,
        id: String,
        kind: ParticipantKind,
        config_schema: serde_json::Value,
    },
}

/// Strictly parse an embedded metadata record. The schema tag selects the
/// variant at parse time, and every version identity inside it selects a
/// variant of its own enum; there is no post-hoc string check anywhere.
pub fn parse_participant_metadata(bytes: &[u8]) -> Result<ParticipantMetadata, MetadataError> {
    serde_json::from_slice(bytes).map_err(MetadataError)
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

    const SCHEMAS: &str = r#"{"bus":"phoxal/bus-abi/v0","launch":"phoxal/participant-launch/v0","robot":"robot/v0","component":"component/v0","simulation":"simulation/v0"}"#;

    fn record(fields: &str) -> Vec<u8> {
        format!(
            r#"{{"schema":"phoxal/participant-metadata/v0","api":"phoxal/robot-api/v0.1","schemas":{SCHEMAS},{fields}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn a_v0_record_parses_into_the_tagged_variant_with_every_boundary_identity() {
        let ParticipantMetadata::V0 {
            api,
            schemas,
            id,
            kind,
            config_schema,
        } = parse_participant_metadata(&record(
            r#""id":"drive","kind":"service","config_schema":{"type":"null"}"#,
        ))
        .expect("the exact document a role macro embeds must parse");

        assert_eq!(api, RobotApi::V0_1);
        assert_eq!(schemas.bus, BusAbi::V0);
        assert_eq!(schemas.launch, LaunchAbi::V0);
        assert_eq!(schemas.robot, RobotSchema::V0);
        assert_eq!(schemas.component, ComponentSchema::V0);
        assert_eq!(schemas.simulation, SimulationSchema::V0);
        assert_eq!(id, "drive");
        assert_eq!(kind, ParticipantKind::Service);
        assert_eq!(config_schema, serde_json::json!({"type": "null"}));
    }

    #[test]
    fn the_root_brain_kind_is_distinct_from_a_service() {
        let ParticipantMetadata::V0 { id, kind, .. } = parse_participant_metadata(&record(
            r#""id":"brain","kind":"brain","config_schema":{"type":"null"}"#,
        ))
        .expect("the exact document `#[phoxal::brain]` embeds must parse");
        assert_eq!(id, "brain");
        assert_eq!(kind, ParticipantKind::Brain);
        assert_ne!(kind, ParticipantKind::Service);
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
        let bytes = br#"{"schema":"phoxal/participant-metadata/v1","api":"phoxal/robot-api/v0.1","id":"drive","kind":"service","config_schema":null}"#;
        assert!(parse_participant_metadata(bytes).is_err());
    }

    #[test]
    fn a_version_identity_from_another_train_is_rejected_by_name() {
        let bytes = format!(
            r#"{{"schema":"phoxal/participant-metadata/v0","api":"phoxal/robot-api/v0.2","schemas":{SCHEMAS},"id":"drive","kind":"service","config_schema":null}}"#
        )
        .into_bytes();
        let message = parse_participant_metadata(&bytes)
            .expect_err("an API revision this train does not speak must not parse")
            .to_string();
        assert!(message.contains("phoxal/robot-api/v0.2"), "{message}");
        assert!(message.contains("phoxal/robot-api/v0.1"), "{message}");
    }

    #[test]
    fn an_unknown_field_is_rejected() {
        assert!(
            parse_participant_metadata(&record(
                r#""id":"drive","kind":"service","config_schema":null,"extra":true"#
            ))
            .is_err()
        );
        // `class` was the tool/checked discriminator and went with the tool
        // concept (#978); a binary still emitting it is stale, not compatible.
        assert!(
            parse_participant_metadata(&record(
                r#""id":"drive","kind":"service","class":"checked","config_schema":null"#
            ))
            .is_err()
        );
        // A framework SemVer is not part of this contract in any form.
        assert!(
            parse_participant_metadata(&record(
                r#""id":"drive","kind":"service","config_schema":null,"framework":"0.53.0""#
            ))
            .is_err()
        );
    }

    #[test]
    fn a_record_missing_a_participant_schema_is_rejected() {
        let bytes = br#"{"schema":"phoxal/participant-metadata/v0","api":"phoxal/robot-api/v0.1","schemas":{"bus":"phoxal/bus-abi/v0","launch":"phoxal/participant-launch/v0","robot":"robot/v0","component":"component/v0"},"id":"drive","kind":"service","config_schema":null}"#;
        assert!(parse_participant_metadata(bytes).is_err());
    }
}
