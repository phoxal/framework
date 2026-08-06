use serde::Deserialize;

/// Exact linker-section metadata schema written by participant macros.
pub const PARTICIPANT_METADATA_SCHEMA: &str = "phoxal/participant-metadata/v0";

/// An API revision identifier as it crosses a process boundary, for example
/// `v0.1`. Opaque: the only operation either side performs on it is equality
/// against its own authoritative constant.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct ApiId(String);

/// A document-schema identifier as it crosses a process boundary, for example
/// `phoxal/bus/v0` or `robot/v0`. Opaque in exactly the same sense as
/// [`ApiId`]; the two are separate types so a schema can never be compared
/// against an API revision.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct SchemaId(String);

macro_rules! opaque_id {
    ($ty:ident) => {
        impl $ty {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl PartialEq<str> for $ty {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $ty {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }
    };
}

opaque_id!(ApiId);
opaque_id!(SchemaId);

/// Every document schema one participant binary speaks. This is the whole
/// compatibility surface between a `phoxal-cli` and a built participant: there
/// is no package-metadata table, no version file, and no framework-SemVer
/// floor anywhere in the process contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ParticipantSchemas {
    /// The bus wire ABI.
    pub bus: SchemaId,
    /// The launch record / environment ABI.
    pub launch: SchemaId,
    /// The authored robot document grammar.
    pub robot: SchemaId,
    /// The authored component document grammar.
    pub component: SchemaId,
    /// The authored simulation document grammar.
    pub simulation: SchemaId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
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

/// The record every participant binary embeds in its `.phoxal_meta` /
/// `__DATA,__phoxal_meta` section at compile time.
///
/// Deserialize-only on purpose. The sole writer is the role macro, which
/// composes the JSON document as a const string in the participant crate;
/// giving this type a `Serialize` impl would invite a second persisted copy of
/// a document that must have exactly one producer.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum ParticipantMetadata {
    #[serde(rename = "phoxal/participant-metadata/v0")]
    V0 {
        api: ApiId,
        schemas: ParticipantSchemas,
        id: String,
        kind: ParticipantKind,
        config_schema: serde_json::Value,
    },
}

/// Strictly parse an embedded metadata record. The schema tag selects the
/// variant at parse time; there is no post-hoc string check.
pub fn parse_participant_metadata(bytes: &[u8]) -> Result<ParticipantMetadata, MetadataError> {
    serde_json::from_slice(bytes).map_err(MetadataError)
}

/// An embedded metadata section that is not a document this framework train
/// understands: malformed JSON, an unknown schema tag, or an unknown field.
#[derive(Debug, thiserror::Error)]
#[error("participant metadata is not a readable phoxal document: {0}")]
pub struct MetadataError(#[from] serde_json::Error);

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMAS: &str = r#"{"bus":"phoxal/bus/v0","launch":"phoxal/participant-launch/v0","robot":"robot/v0","component":"component/v0","simulation":"simulation/v0"}"#;

    fn record(fields: &str) -> Vec<u8> {
        format!(r#"{{"schema":"{PARTICIPANT_METADATA_SCHEMA}","api":"v0.1","schemas":{SCHEMAS},{fields}}}"#)
            .into_bytes()
    }

    #[test]
    fn a_v0_record_parses_into_the_tagged_variant_with_every_boundary_identifier() {
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

        assert_eq!(api, "v0.1");
        assert_eq!(schemas.bus, "phoxal/bus/v0");
        assert_eq!(schemas.launch, "phoxal/participant-launch/v0");
        assert_eq!(schemas.robot, "robot/v0");
        assert_eq!(schemas.component, "component/v0");
        assert_eq!(schemas.simulation, "simulation/v0");
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
    fn an_unknown_schema_tag_is_rejected() {
        let bytes = br#"{"schema":"phoxal/participant-metadata/v1","api":"v0.1","id":"drive","kind":"service","config_schema":null}"#;
        assert!(parse_participant_metadata(bytes).is_err());
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
        let bytes = br#"{"schema":"phoxal/participant-metadata/v0","api":"v0.1","schemas":{"bus":"phoxal/bus/v0","launch":"phoxal/participant-launch/v0","robot":"robot/v0","component":"component/v0"},"id":"drive","kind":"service","config_schema":null}"#;
        assert!(parse_participant_metadata(bytes).is_err());
    }
}
