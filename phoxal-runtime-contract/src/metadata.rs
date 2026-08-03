use serde::Deserialize;

/// Exact linker-section metadata schema written by participant macros.
pub const PARTICIPANT_METADATA_SCHEMA: &str = "phoxal/participant-metadata/v0";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantKind {
    Service,
    Driver,
    Simulator,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ParticipantMetadata {
    pub schema: String,
    pub id: String,
    pub kind: ParticipantKind,
    pub config_schema: serde_json::Value,
}

/// Strictly parse and validate an embedded metadata record.
pub fn parse_participant_metadata(bytes: &[u8]) -> Result<ParticipantMetadata, MetadataError> {
    let metadata: ParticipantMetadata = serde_json::from_slice(bytes)?;
    if metadata.schema != PARTICIPANT_METADATA_SCHEMA {
        return Err(MetadataError::Schema(metadata.schema));
    }
    Ok(metadata)
}

#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error("participant metadata JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported participant metadata schema '{0}'")]
    Schema(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_metadata_shape_round_trips() {
        let bytes = br#"{"schema":"phoxal/participant-metadata/v0","id":"drive","kind":"service","config_schema":{"type":"null"}}"#;
        let metadata = parse_participant_metadata(bytes).unwrap();
        assert_eq!(metadata.kind, ParticipantKind::Service);
        assert_eq!(metadata.id, "drive");
    }

    #[test]
    fn unknown_schema_and_fields_are_rejected() {
        assert!(matches!(
            parse_participant_metadata(
                br#"{"schema":"v1","id":"drive","kind":"service","config_schema":null}"#
            ),
            Err(MetadataError::Schema(_))
        ));
        assert!(parse_participant_metadata(br#"{"schema":"phoxal/participant-metadata/v0","id":"drive","kind":"service","config_schema":null,"extra":true}"#).is_err());
        // `class` was the tool/checked discriminator and went with the tool
        // concept (#978); a binary still emitting it is stale, not compatible.
        assert!(parse_participant_metadata(br#"{"schema":"phoxal/participant-metadata/v0","id":"drive","kind":"service","class":"checked","config_schema":null}"#).is_err());
    }
}
