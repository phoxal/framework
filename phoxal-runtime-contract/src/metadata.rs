//! The read side of the embedded participant-metadata document.
//!
//! Every participant binary carries this record in a linker section, and it is
//! the whole compatibility surface between a `phoxal-cli` and that binary. The
//! matching writer is [`crate::emit`].

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

/// The one topology requirement a participant binary may currently declare.
///
/// This is intentionally a closed, narrow enum rather than a generic dispatch
/// table keyed by package or participant id. A binary that declares
/// [`Self::DifferentialDriveVelocity`] can only run in a runtime document whose
/// robot has differential kinematics and velocity-command motors on every
/// configured drive side actuator.
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
        api: RobotApi,
        schemas: ParticipantSchemas,
        id: String,
        kind: ParticipantKind,
        /// The optional static topology contract. Absent means that this
        /// legacy-compatible metadata record has no additional requirement.
        #[serde(default)]
        requirement: Option<ParticipantRequirement>,
        config_schema: serde_json::Value,
    },
}

impl ParticipantMetadata {
    /// Strictly parse the bytes of an embedded metadata section.
    ///
    /// The schema tag selects the variant at parse time, and every version
    /// identity inside it selects a variant of its own enum; there is no
    /// post-hoc string check anywhere.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MetadataError> {
        serde_json::from_slice(bytes).map_err(MetadataError)
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

    const SCHEMAS: &str = r#"{"bus":"phoxal/bus-abi/v0","launch":"phoxal/participant-launch/v0","robot":"phoxal/robot/v0","component":"phoxal/component/v0","simulation":"phoxal/simulation/v0"}"#;

    fn record(fields: &str) -> Vec<u8> {
        format!(
            r#"{{"schema":"phoxal/participant-metadata/v0","api":"phoxal/robot-api/v0.2","schemas":{SCHEMAS},{fields}}}"#
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
            requirement,
            config_schema,
        } = ParticipantMetadata::from_bytes(&record(
            r#""id":"drive","kind":"service","config_schema":{"type":"null"}"#,
        ))
        .expect("the exact document a role macro embeds must parse");

        assert_eq!(api, RobotApi::V0_2);
        assert_eq!(schemas.bus, BusAbi::V0);
        assert_eq!(schemas.launch, LaunchAbi::V0);
        assert_eq!(schemas.robot, RobotSchema::V0);
        assert_eq!(schemas.component, ComponentSchema::V0);
        assert_eq!(schemas.simulation, SimulationSchema::V0);
        assert_eq!(id, "drive");
        assert_eq!(kind, ParticipantKind::Service);
        assert_eq!(requirement, None);
        assert_eq!(config_schema, serde_json::json!({"type": "null"}));
    }

    #[test]
    fn the_root_brain_kind_is_distinct_from_a_service() {
        let ParticipantMetadata::V0 { id, kind, .. } = ParticipantMetadata::from_bytes(&record(
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

    /// The 0.53-era embedded document, byte for byte: a bare `v0.1` API
    /// revision, an unqualified `phoxal/bus/v0`, and unnamespaced authored
    /// grammars. Deliberately a frozen literal rather than anything composed
    /// from this crate's constants - it is a record of what that train actually
    /// shipped, and a later rename here must not quietly rewrite the artifact
    /// the test claims to be about.
    ///
    /// The diagnostic has to do both halves of the operator's job at once: name
    /// the token the stale binary carries, so it is identifiable, and name the
    /// token this train expects, so the fix is obvious without reading source.
    const ERA_0_53_RECORD: &str = r#"{"schema":"phoxal/participant-metadata/v0","api":"v0.1","schemas":{"bus":"phoxal/bus/v0","launch":"phoxal/participant-launch/v0","robot":"robot/v0","component":"component/v0","simulation":"simulation/v0"},"id":"drive","kind":"service","config_schema":null}"#;

    #[test]
    fn a_0_53_era_binary_is_rejected_naming_its_token_and_the_expected_spelling() {
        let message = ParticipantMetadata::from_bytes(ERA_0_53_RECORD.as_bytes())
            .expect_err("a 0.53-era binary is not a peer this train can speak to")
            .to_string();

        // Backticked, so this asserts the bare `v0.1` the stale binary carries
        // and is not satisfied by the `v0.1` tail of the expected spelling.
        assert!(
            message.contains("`v0.1`"),
            "the diagnostic must name the token the stale binary carries: {message}"
        );
        assert!(
            message.contains(RobotApi::V0_1.as_str()),
            "the diagnostic must name the spelling this train expects: {message}"
        );
        assert_eq!(
            RobotApi::V0_1.as_str(),
            "phoxal/robot-api/v0.1",
            "the expected spelling is pinned here too, so a rename cannot make \
             the assertion above vacuous"
        );
    }

    /// The same upgrade, one field deeper: with the API revision made current,
    /// the stale `schemas` entries are caught by name too rather than accepted
    /// as unknown-but-harmless strings. Serde reports the first one it reaches,
    /// which is `bus`.
    #[test]
    fn a_0_53_era_schema_spelling_is_rejected_by_name() {
        let bytes = ERA_0_53_RECORD.replace(r#""api":"v0.1""#, r#""api":"phoxal/robot-api/v0.1""#);

        let message = ParticipantMetadata::from_bytes(bytes.as_bytes())
            .expect_err("the old bus spelling is not a version this train speaks")
            .to_string();

        assert!(message.contains("`phoxal/bus/v0`"), "{message}");
        assert!(message.contains(BusAbi::V0.as_str()), "{message}");
    }

    /// And with `bus` current as well, the authored-grammar rename to the
    /// namespaced form is what the stale binary trips on. This is the assertion
    /// that would have been silently satisfied if `robot/v0` were still legal.
    #[test]
    fn a_0_53_era_authored_grammar_spelling_is_rejected_by_name() {
        let bytes = ERA_0_53_RECORD
            .replace(r#""api":"v0.1""#, r#""api":"phoxal/robot-api/v0.1""#)
            .replace(r#""bus":"phoxal/bus/v0""#, r#""bus":"phoxal/bus-abi/v0""#);

        let message = ParticipantMetadata::from_bytes(bytes.as_bytes())
            .expect_err("the unnamespaced robot grammar is not a version this train speaks")
            .to_string();

        assert!(message.contains("`robot/v0`"), "{message}");
        assert!(message.contains(RobotSchema::V0.as_str()), "{message}");
        assert_eq!(RobotSchema::V0.as_str(), "phoxal/robot/v0", "{message}");
    }

    #[test]
    fn an_unknown_field_is_rejected() {
        assert!(
            ParticipantMetadata::from_bytes(&record(
                r#""id":"drive","kind":"service","config_schema":null,"extra":true"#
            ))
            .is_err()
        );
        // There is no `class` discriminator in this contract; a binary still
        // emitting one is stale, not compatible.
        assert!(
            ParticipantMetadata::from_bytes(&record(
                r#""id":"drive","kind":"service","class":"checked","config_schema":null"#
            ))
            .is_err()
        );
        // A framework SemVer is not part of this contract in any form.
        assert!(
            ParticipantMetadata::from_bytes(&record(
                r#""id":"drive","kind":"service","config_schema":null,"framework":"0.53.0""#
            ))
            .is_err()
        );
    }

    #[test]
    fn a_record_missing_a_participant_schema_is_rejected() {
        let bytes = br#"{"schema":"phoxal/participant-metadata/v0","api":"phoxal/robot-api/v0.1","schemas":{"bus":"phoxal/bus-abi/v0","launch":"phoxal/participant-launch/v0","robot":"phoxal/robot/v0","component":"phoxal/component/v0"},"id":"drive","kind":"service","config_schema":null}"#;
        assert!(ParticipantMetadata::from_bytes(bytes).is_err());
    }

    #[test]
    fn an_unknown_requirement_is_rejected() {
        let bytes = record(
            r#""id":"drive","kind":"service","requirement":"drive_everything","config_schema":null"#,
        );
        assert!(ParticipantMetadata::from_bytes(&bytes).is_err());
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
    fn a_valid_record_without_requirement_remains_legacy_compatible() {
        let ParticipantMetadata::V0 { requirement, .. } = ParticipantMetadata::from_bytes(&record(
            r#""id":"drive","kind":"service","config_schema":null"#,
        ))
        .expect("v0 metadata without the additive field remains readable");
        assert_eq!(requirement, None);
    }
}
