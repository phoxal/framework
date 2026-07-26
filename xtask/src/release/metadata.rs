//! Compile-time participant metadata extraction.
//!
//! The participant attribute embeds one JSON manifest per participant binary in
//! a dedicated linker section - `__DATA,__phoxal_meta` on Mach-O, `.phoxal_api_meta`
//! everywhere else (`phoxal-macros/src/authoring.rs`'s `link_section_attrs`).
//! This module reads the section's bytes straight out of the object file,
//! without ever executing the artifact. It is format- and architecture-agnostic
//! (via the `object` crate), which is load-bearing for its two consumers: the
//! release-PR coherence gate builds each participant for the host and reads
//! the section from that debug binary (`package::build_and_extract_metadata`),
//! while `release package` validates the section on the just-built release
//! binary for whichever target it is packaging - together covering every
//! aarch64/x86_64 ELF and Mach-O shape the framework ships without a fresh
//! native rebuild per target.
//!
//! This module owns only the object-file section-BYTES extraction (an
//! `object`-crate walk over an ELF/Mach-O binary); the JSON shape
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
/// hard error. `describe` is an arbitrary caller-chosen label for the object
/// file's source (a file path, or another descriptive tag) used in error
/// messages.
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
/// (an ELF/Mach-O binary of any target architecture - this is how `release
/// package` reads the section out of a just-built, possibly cross-compiled,
/// binary without executing it). Reads nothing, runs nothing. An `Api = ()`
/// participant still emits the section, with an empty contract list; a binary
/// with no section at all is a hard error (see
/// [`extract_participant_metadata_section_from_bytes`] and
/// [`parse_participant_metadata_section`]).
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
    /// disk, and assert the simulator-owned world-clock publisher is present.
    #[test]
    fn extracts_real_webots_controller_clock_metadata() -> Result<()> {
        let workspace = Workspace::discover()?;
        let package_name = "phoxal-simulator-webots-controller";
        let status = Command::new("cargo")
            .args(["build", "--quiet", "-p", package_name])
            .current_dir(workspace.root())
            .status()
            .context("failed to spawn cargo build for webots controller")?;
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
        assert!(meta.contracts.contains(&ParticipantMetaContract {
            role: "publish".to_string(),
            version: "v0.1".to_string(),
            contract: "simulation::Clock".to_string(),
            external: false,
        }));
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
    /// both format and architecture. This is exactly what `release package`
    /// relies on when it validates a just-built, possibly cross-compiled,
    /// target binary without a native rebuild.
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
