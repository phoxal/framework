//! Compile-time participant metadata extraction (X-tools slice).
//!
//! The participant attribute embeds one JSON manifest per participant binary in
//! a dedicated linker section - `__DATA,__phoxal_meta` on Mach-O, `.phoxal_api_meta`
//! everywhere else (`phoxal-macros/src/authoring.rs`'s `link_section_attrs`).
//! `xtask` no longer runs `emit-apis` (that runtime subcommand is being retired
//! separately, see the Cleanup slice) to learn a participant's contract
//! surface: it reads the section's bytes straight out of the object file,
//! without ever executing the artifact. This module is format- and
//! architecture-agnostic (via the `object` crate), which is load-bearing for
//! the release pipeline: the `catalog-publish` job runs on an x86_64 Linux host
//! but the binaries it indexes are the cross-compiled artifacts actually being
//! shipped - aarch64/x86_64 ELF *and* aarch64/x86_64 Mach-O. Parsing the
//! section out of *those* binaries (see `package::extract_metadata_from_packaged`,
//! which reads every target straight out of the release tarballs) rather than
//! from a fresh native rebuild is what makes publish-time coherence reflect the
//! exact bytes that users download.
//!
//! This module owns only the object-file section-BYTES extraction (an
//! `object`-crate walk over an ELF/Mach-O binary or tarball); the JSON shape
//! itself - `{"role","version","contract","external"}` - is deserialized
//! via the shared [`phoxal::participant::metadata`] type, per the
//! coherence-gate design doc §5 ("move the parser alongside the rule into
//! `phoxal`"), so this crate and `phoxal-cli` read exactly the same shape
//! instead of each hand-copying the JSON schema.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use object::{Object, ObjectSection};

pub(crate) use phoxal::participant::metadata::ParticipantMeta;
// Only named directly by test fixtures (`ParticipantMetaContract { .. }`
// literals); production code moves `ParticipantMeta::contracts` directly into
// coherence surfaces.
#[cfg(test)]
pub(crate) use phoxal::participant::metadata::ParticipantMetaContract;

/// The linker section names a participant attribute places its metadata
/// static under, tried in order (`phoxal-macros/src/authoring.rs::link_section_attrs`).
/// `object`'s generic [`Object::section_by_name`] matches on the section name
/// alone (Mach-O segment qualification is not part of the match), so no
/// per-format branching is needed here - the two candidate names are simply
/// disjoint across the object formats this framework ships binaries for.
const SECTION_NAMES: [&str; 2] = [".phoxal_api_meta", "__phoxal_meta"];

/// Parses `object_bytes` as an object file and returns the bytes of its
/// participant metadata section, trying each candidate section
/// name in [`SECTION_NAMES`] in turn. `Ok(None)` means the object file parsed
/// but is not a participant: every participant attribute emits the section,
/// including `Api = ()`. A malformed/unrecognized *object file* is still a
/// hard error. `describe` names the source (a file path, or a `pkg@target from
/// tarball` label) for error messages.
pub(crate) fn extract_participant_metadata_section_from_bytes(
    object_bytes: &[u8],
    describe: &str,
) -> Result<Option<Vec<u8>>> {
    let file = object::File::parse(object_bytes)
        .with_context(|| format!("{describe} is not a recognized object file (ELF/Mach-O/...)"))?;

    for name in SECTION_NAMES {
        if let Some(section) = file.section_by_name(name) {
            let bytes = section
                .data()
                .with_context(|| format!("failed to read section '{name}' data from {describe}"))?;
            return Ok(Some(bytes.to_vec()));
        }
    }

    Ok(None)
}

/// Parses an already-extracted metadata section. Every participant, including
/// `Api = ()`, must carry the coordinated metadata format.
pub(crate) fn parse_participant_metadata_section(
    section: Option<&[u8]>,
    describe: &str,
) -> Result<ParticipantMeta> {
    let bytes = section.with_context(|| format!("{describe} has no phoxal metadata section"))?;
    phoxal::participant::metadata::parse_participant_metadata(bytes)
        .with_context(|| format!("phoxal API metadata section in {describe} is not valid JSON"))
}

/// Parses the embedded participant metadata out of an in-memory object file
/// (an ELF/Mach-O binary of any target architecture - this is how the
/// `catalog-publish` job on an x86_64 host reads the section out of a
/// cross-compiled aarch64 binary). Reads nothing, runs nothing. A binary with
/// no section at all (an `Api = ()` participant - see
/// [`extract_participant_metadata_section_from_bytes`])
/// parses as an empty contract list, not an error.
pub(crate) fn extract_participant_metadata_from_bytes(
    object_bytes: &[u8],
    describe: &str,
) -> Result<ParticipantMeta> {
    let section = extract_participant_metadata_section_from_bytes(object_bytes, describe)?;
    parse_participant_metadata_section(section.as_deref(), describe)
}

/// Extracts and parses `binary_path`'s embedded participant metadata: reads the
/// compiled-in linker section straight off the object file, never executing it.
pub(crate) fn extract_participant_metadata(binary_path: &Path) -> Result<ParticipantMeta> {
    let data = fs::read(binary_path)
        .with_context(|| format!("failed to read {}", binary_path.display()))?;
    extract_participant_metadata_from_bytes(&data, &binary_path.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::workspace::Workspace;

    /// End-to-end proof (X-tools slice acceptance criteria): build a real
    /// participant, extract its section from the actual built artifact on
    /// disk, and assert the parsed contracts match what `service/battery/src/main.rs`
    /// declares (`Api { state: Publisher<api::battery::State> }`), recorded
    /// as the RESOLVED, SPLIT `version`/`contract` (`"v2"` /
    /// `"battery::State"` - `battery::State` lives on the standalone `v2`
    /// version, D1's ground-breaker), not the source-written
    /// `api::battery::State` (F2-names), and not a joined name (coherence-gate
    /// design doc §2).
    #[test]
    fn extracts_real_battery_binary_metadata() -> Result<()> {
        let workspace = Workspace::discover()?;
        let package_name = "phoxal-service-battery";
        let status = Command::new("cargo")
            .args(["build", "--quiet", "-p", package_name])
            .current_dir(workspace.root())
            .status()
            .context("failed to spawn cargo build for phoxal-service-battery")?;
        assert!(status.success(), "cargo build -p {package_name} failed");

        let binary_path = workspace
            .target_dir()
            .join("debug")
            .join(format!("{package_name}{}", std::env::consts::EXE_SUFFIX));
        assert!(
            binary_path.is_file(),
            "expected built binary at {}",
            binary_path.display()
        );

        let meta = extract_participant_metadata(&binary_path)?;
        assert_eq!(meta.participant_api, "Api");
        assert_eq!(meta.config_schema["type"], "null");
        assert_eq!(
            meta.contracts,
            vec![ParticipantMetaContract {
                role: "publish".to_string(),
                version: "v2".to_string(),
                contract: "battery::State".to_string(),
                external: false,
            }]
        );
        Ok(())
    }

    /// A privileged tool defaults to `Api = ()` AND (since the shared
    /// configless-tool fix) `Config = ()`, but the participant attribute
    /// still emits config schema metadata and an empty contract list. A `()`
    /// config schema is `{"type":"null"}`, not `"object"`: a configless tool
    /// must start with `PHOXAL_CONFIG` ABSENT, which only `()`'s
    /// `Deserialize` accepts (a zero-field struct expects a map).
    #[test]
    fn api_unit_binary_still_carries_metadata() -> Result<()> {
        let workspace = Workspace::discover()?;
        let package_name = "phoxal-tool-joypad";
        let status = Command::new("cargo")
            .args(["build", "--quiet", "-p", package_name])
            .current_dir(workspace.root())
            .status()
            .context("failed to spawn cargo build for phoxal-tool-joypad")?;
        assert!(status.success(), "cargo build -p {package_name} failed");

        let binary_path = workspace
            .target_dir()
            .join("debug")
            .join(format!("{package_name}{}", std::env::consts::EXE_SUFFIX));

        let meta = extract_participant_metadata(&binary_path)?;
        assert!(meta.contracts.is_empty());
        assert_eq!(meta.participant_api, "()");
        assert_eq!(meta.config_schema["type"], "null");
        Ok(())
    }

    #[test]
    fn malformed_object_file_fails_with_a_clear_error() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        let not_an_object_file = dir.path().join("not-a-binary");
        fs::write(&not_an_object_file, b"not an object file")?;

        let err = extract_participant_metadata(&not_an_object_file).unwrap_err();
        assert!(
            err.to_string().contains("not a recognized object file"),
            "{err}"
        );
        Ok(())
    }

    /// Synthesizes an object file of a given format/architecture carrying the
    /// phoxal metadata section, so the reader is exercised against object
    /// shapes that are NOT the test host's native one.
    fn synthesize_object(
        format: object::BinaryFormat,
        arch: object::Architecture,
        section_name: &[u8],
        segment_name: &[u8],
        payload: &[u8],
    ) -> Vec<u8> {
        use object::write::Object;
        let mut obj = Object::new(format, arch, object::Endianness::Little);
        let section = obj.add_section(
            segment_name.to_vec(),
            section_name.to_vec(),
            object::SectionKind::ReadOnlyData,
        );
        obj.append_section_data(section, payload, 1);
        obj.write().expect("synthesize object file")
    }

    /// Cross-FORMAT / cross-ARCH extraction proof (#3): the reader is not
    /// host-locked. Parses the phoxal section out of both an aarch64 ELF and an
    /// x86_64 Mach-O built entirely in memory - on any given test host at least
    /// one of these is a foreign object format, and neither matches the host on
    /// both format and architecture. This is exactly what the `catalog-publish`
    /// x86_64-Linux job relies on when it reads the section out of the shipped
    /// aarch64 (ELF) and Apple (Mach-O) release binaries.
    #[test]
    fn extracts_metadata_from_foreign_format_and_arch_object_files() -> Result<()> {
        let payload = br#"{"participant_api":"Api","contracts":[{"role":"publish","version":"v1","contract":"drive::State","external":false}],"config_schema":{"type":"null"}}"#;
        let expected = vec![ParticipantMetaContract {
            role: "publish".to_string(),
            version: "v1".to_string(),
            contract: "drive::State".to_string(),
            external: false,
        }];

        // aarch64 ELF (Linux robot / release binary shape), `.phoxal_api_meta`.
        let elf = synthesize_object(
            object::BinaryFormat::Elf,
            object::Architecture::Aarch64,
            b".phoxal_api_meta",
            b"",
            payload,
        );
        let from_elf = extract_participant_metadata_from_bytes(&elf, "synthetic aarch64 ELF")?;
        assert_eq!(from_elf.contracts, expected);

        // x86_64 Mach-O (Apple release binary shape), `__DATA,__phoxal_meta`.
        let macho = synthesize_object(
            object::BinaryFormat::MachO,
            object::Architecture::X86_64,
            b"__phoxal_meta",
            b"__DATA",
            payload,
        );
        let from_macho =
            extract_participant_metadata_from_bytes(&macho, "synthetic x86_64 Mach-O")?;
        assert_eq!(from_macho.contracts, expected);
        Ok(())
    }

    /// A foreign-format object file with no phoxal section is not a participant.
    #[test]
    fn foreign_object_without_section_is_rejected() -> Result<()> {
        let elf = synthesize_object(
            object::BinaryFormat::Elf,
            object::Architecture::Aarch64,
            b".some_other_section",
            b"",
            b"unrelated",
        );
        let err =
            extract_participant_metadata_from_bytes(&elf, "synthetic aarch64 ELF").unwrap_err();
        assert!(err.to_string().contains("no phoxal metadata section"));
        Ok(())
    }
}
