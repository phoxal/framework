//! Shared deserialization of the participant linker-section metadata JSON.
//!
//! A participant attribute (`phoxal-macros/src/authoring.rs`) embeds one JSON
//! object per participant binary in a dedicated linker section - see
//! `phoxal::participant::api::__meta` for the const-eval mechanism that
//! resolves it. The shape is `{"id", "config_schema"}`: which participant this
//! binary implements, and the configuration it accepts. It is strict; this
//! pre-1.0 writer/parser cut has no compatibility fallback, so unknown fields
//! are rejected rather than partially understood.
//!
//! This module owns ONLY the JSON shape, not the object-file section-BYTES
//! extraction, which stays host-specific (an `object`-crate walk over an
//! ELF/Mach-O binary) in the consumer: `phoxal-cli`'s own
//! `participant_metadata.rs`. It extracts the section's raw bytes its own way,
//! then hands them to [`parse_participant_metadata`] here so the JSON shape has
//! exactly one definition.

use serde::Deserialize;

/// The embedded metadata manifest for one participant. This is a strict
/// pre-1.0 format whose writer and parsers move in lockstep.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ParticipantMeta {
    /// The participant's declared identity - the `id` given to its role
    /// attribute, e.g. `#[phoxal::service(id = "drive")]`. This is what lets a
    /// consumer name the participant a binary implements without inferring it
    /// from a path.
    pub id: String,
    /// The participant's compile-time Draft 2020-12 config schema.
    pub config_schema: serde_json::Value,
}

/// Parses the metadata section's raw JSON bytes - already extracted from an
/// object file's linker section by the caller (see the module docs for why
/// that extraction stays out of this crate) - into a [`ParticipantMeta`].
pub fn parse_participant_metadata(section_bytes: &[u8]) -> serde_json::Result<ParticipantMeta> {
    serde_json::from_slice(section_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_current_section_shape() {
        let json = br#"{"id":"drive","config_schema":{"type":"object","properties":{"speed":{"type":"number"}}}}"#;
        let meta = parse_participant_metadata(json).expect("valid metadata JSON");
        assert_eq!(meta.id, "drive");
        assert_eq!(meta.config_schema["type"], "object");
    }

    /// A configless participant still names itself. `config = ()` serialises as
    /// the null schema, so identity is the only field carrying information.
    #[test]
    fn a_configless_participant_still_carries_its_id() {
        let json = br#"{"id":"joypad","config_schema":{"type":"null"}}"#;
        let meta = parse_participant_metadata(json).expect("valid metadata JSON");
        assert_eq!(meta.id, "joypad");
        assert_eq!(meta.config_schema["type"], "null");
    }

    #[test]
    fn malformed_json_is_a_clear_error() {
        let err = parse_participant_metadata(b"not json").unwrap_err();
        assert!(err.to_string().contains("expected"));
    }

    #[test]
    fn missing_config_schema_is_rejected() {
        let err = parse_participant_metadata(br#"{"id":"drive"}"#).unwrap_err();
        assert!(err.to_string().contains("config_schema"));
    }

    #[test]
    fn missing_id_is_rejected() {
        let err = parse_participant_metadata(br#"{"config_schema":{"type":"null"}}"#).unwrap_err();
        assert!(err.to_string().contains("id"));
    }

    /// Unknown metadata is rejected rather than silently ignored.
    #[test]
    fn an_unknown_metadata_field_is_rejected() {
        let json = br#"{"id":"drive","unexpected":[],"config_schema":{"type":"null"}}"#;
        let err = parse_participant_metadata(json).unwrap_err();
        assert!(err.to_string().contains("unexpected"), "{err}");
    }
}
