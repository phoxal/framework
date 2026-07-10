//! Shared deserialization of the `#[derive(phoxal::Api)]` linker-section
//! metadata JSON (coherence-gate design doc §2/§5).
//!
//! `#[derive(phoxal::Api)]` (`phoxal-macros/src/authoring.rs`) embeds one JSON
//! manifest per participant binary in a dedicated linker section - see
//! `phoxal::participant::api::__meta` for the const-eval mechanism that
//! resolves it. The entry shape is `{"role","generation","contract","external"}`:
//! generation and contract are recorded as SEPARATE fields (never a joined
//! name a reader would have to parse), and `external` defaults to `false` so
//! older sections/hand-written fixtures without the key still parse.
//!
//! This module owns ONLY the JSON shape, not the object-file section-BYTES
//! extraction, which stays host-specific (an `object`-crate walk over an
//! ELF/Mach-O binary or a release tarball) in each consumer:
//! `xtask/src/release/metadata.rs` for the release pipeline (`cargo xtask
//! catalog generate`/`release package`), and `phoxal-cli`'s own
//! `participant_metadata.rs` for the CLI (not yet switched to this shared
//! type - a later slice, per the coherence-gate design doc §5). Both extract
//! the section's raw bytes their own way, then hand them to
//! [`parse_participant_metadata`] here so the JSON shape has exactly one
//! definition shared by every reader.

use serde::Deserialize;

/// One `{"role","generation","contract","external"}` entry from the embedded
/// manifest: one participant `Api` struct field's role for one contract (a
/// `Server<Req, Resp>`/`Querier<Req, Resp>` field contributes two entries -
/// one per side). Deduplicated per `(role, contract)` by the derive
/// (`phoxal-macros/src/authoring.rs`); there is no `field` key - dedup already
/// made it name an arbitrary first field, and nothing read it (coherence-gate
/// design doc §2).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ParticipantMetaContract {
    /// `"publish"`, `"subscribe"`, `"serve"`, or `"ask"`
    /// (`phoxal::participant::ContractRole`, snake_case).
    pub role: String,
    /// The contract's generation, e.g. `"y2026_1"`
    /// (`<Body as phoxal_bus::ContractBody>::GENERATION`).
    pub generation: String,
    /// The contract's path within its generation, e.g. `"drive::Target"`
    /// (`<Body as phoxal_bus::ContractBody>::CONTRACT`). The **logical
    /// contract** for coherence purposes is this field alone - two entries
    /// with the same `contract` but different `generation` name the same
    /// logical contract at different generations; the version-qualified
    /// identity is the `generation`/`contract` join.
    pub contract: String,
    /// Whether this edge is excused from the coherence check
    /// (`#[phoxal(external)]`, coherence-gate design doc §1): the counterpart
    /// is known, at authoring time, to legitimately live outside the checked
    /// participant set. Only ever `true` for a `subscribe`/`ask` entry - the
    /// derive rejects the attribute on `publish`/`serve` fields. `false` by
    /// default so a section/fixture written before this field existed still
    /// parses.
    #[serde(default)]
    pub external: bool,
}

/// The embedded metadata manifest for one `#[derive(phoxal::Api)]` struct:
/// `{"participant_api":"<StructName>","contracts":[...]}`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ParticipantMeta {
    /// The `Api` struct's type name (e.g. `"Api"`), recorded purely as
    /// provenance by `#[derive(phoxal::Api)]`.
    pub participant_api: String,
    pub contracts: Vec<ParticipantMetaContract>,
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
        let json = br#"{"participant_api":"Api","contracts":[
            {"role":"subscribe","generation":"y2026_1","contract":"drive::Target","external":false},
            {"role":"publish","generation":"y2026_1","contract":"drive::State","external":false}
        ]}"#;
        let meta = parse_participant_metadata(json).expect("valid metadata JSON");
        assert_eq!(meta.participant_api, "Api");
        assert_eq!(
            meta.contracts,
            vec![
                ParticipantMetaContract {
                    role: "subscribe".to_string(),
                    generation: "y2026_1".to_string(),
                    contract: "drive::Target".to_string(),
                    external: false,
                },
                ParticipantMetaContract {
                    role: "publish".to_string(),
                    generation: "y2026_1".to_string(),
                    contract: "drive::State".to_string(),
                    external: false,
                },
            ]
        );
    }

    #[test]
    fn external_true_is_recorded() {
        let json = br#"{"participant_api":"Api","contracts":[
            {"role":"subscribe","generation":"y2026_1","contract":"drive::Target","external":true}
        ]}"#;
        let meta = parse_participant_metadata(json).expect("valid metadata JSON");
        assert!(meta.contracts[0].external);
    }

    /// A section written before `external` existed (or a hand-written
    /// fixture that omits it) still parses, defaulting to `false` - the
    /// module docs' forward/backward-tolerance guarantee.
    #[test]
    fn missing_external_key_defaults_to_false() {
        let json = br#"{"participant_api":"Api","contracts":[
            {"role":"publish","generation":"y2026_1","contract":"drive::State"}
        ]}"#;
        let meta = parse_participant_metadata(json).expect("valid metadata JSON");
        assert!(!meta.contracts[0].external);
    }

    #[test]
    fn empty_contracts_list_parses() {
        let json = br#"{"participant_api":"()","contracts":[]}"#;
        let meta = parse_participant_metadata(json).expect("valid metadata JSON");
        assert!(meta.contracts.is_empty());
    }

    #[test]
    fn malformed_json_is_a_clear_error() {
        let err = parse_participant_metadata(b"not json").unwrap_err();
        assert!(err.to_string().contains("expected"));
    }
}
