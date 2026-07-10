//! Compile-time participant metadata extraction (X-tools slice).
//!
//! `#[derive(phoxal::Api)]` embeds one JSON manifest per participant binary in
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
//! which reads them straight out of the released tarballs) rather than from a
//! fresh native rebuild is what keeps the catalog metadata physically
//! inseparable from the bytes that land on the robot.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use object::{Object, ObjectSection};
use serde::Deserialize;

/// The linker section names `#[derive(phoxal::Api)]` places its metadata
/// static under, tried in order (`phoxal-macros/src/authoring.rs::link_section_attrs`).
/// `object`'s generic [`Object::section_by_name`] matches on the section name
/// alone (Mach-O segment qualification is not part of the match), so no
/// per-format branching is needed here - the two candidate names are simply
/// disjoint across the object formats this framework ships binaries for.
const SECTION_NAMES: [&str; 2] = [".phoxal_api_meta", "__phoxal_meta"];

/// One `{"field","role","contract"}` entry from the embedded manifest: one
/// participant `Api` struct field's role for one contract (a `Server<Req,
/// Resp>` field contributes two entries - one per side).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct ParticipantMetaContract {
    /// The `Api` struct field name that declared this contract (provenance
    /// only - not part of the contract's identity).
    #[allow(dead_code)]
    pub field: String,
    /// `"publish"`, `"subscribe"`, `"serve"`, or `"ask"`
    /// (`phoxal::participant::ContractRole`, snake_case).
    pub role: String,
    /// The contract's RESOLVED, generation-qualified wire key - `<Body as
    /// phoxal_bus::ContractBody>::TOPIC` (e.g. `"y2026_1/drive/target"`),
    /// not the body type as written in the participant's source. Every
    /// participant writes `use phoxal_api::y2026_N as api;`, so a
    /// source-text string like `"api::drive::Target"` would have the
    /// generation erased and could not distinguish a `y2026_1` contract from
    /// a same-named `y2026_7` one; `TOPIC` is the identity that actually
    /// disambiguates generations on the wire (D1), so it is what
    /// `#[derive(phoxal::Api)]` records here.
    pub contract: String,
}

/// The embedded metadata manifest for one `#[derive(phoxal::Api)]` struct:
/// `{"participant_api":"<StructName>","contracts":[...]}`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct ParticipantMeta {
    /// The `Api` struct's type name (e.g. `"Api"`), recorded purely as
    /// provenance by `#[derive(phoxal::Api)]`.
    #[allow(dead_code)]
    pub participant_api: String,
    pub contracts: Vec<ParticipantMetaContract>,
}

/// Parses `object_bytes` as an object file and returns the bytes of its
/// `#[derive(phoxal::Api)]` metadata section, trying each candidate section
/// name in [`SECTION_NAMES`] in turn. `Ok(None)` means the object file parsed
/// fine but carries no such section at all - the expected, valid shape for a
/// participant whose `Api` is `()` (privileged tools default to this; see
/// `phoxal-macros/src/authoring.rs::ParticipantKind::default_api`), which
/// never derives `#[derive(phoxal::Api)]` and so never emits the linker
/// section in the first place. A malformed/unrecognized *object file* is still
/// a hard error. `describe` names the source (a file path, or a
/// `pkg@target from tarball` label) for error messages.
fn read_meta_section(object_bytes: &[u8], describe: &str) -> Result<Option<Vec<u8>>> {
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

/// Parses the embedded participant metadata out of an in-memory object file
/// (an ELF/Mach-O binary of any target architecture - this is how the
/// `catalog-publish` job on an x86_64 host reads the section out of a
/// cross-compiled aarch64 binary). Reads nothing, runs nothing. A binary with
/// no section at all (an `Api = ()` participant - see [`read_meta_section`])
/// parses as an empty contract list, not an error.
pub(crate) fn extract_participant_metadata_from_bytes(
    object_bytes: &[u8],
    describe: &str,
) -> Result<ParticipantMeta> {
    let Some(bytes) = read_meta_section(object_bytes, describe)? else {
        return Ok(ParticipantMeta {
            participant_api: "()".to_string(),
            contracts: Vec::new(),
        });
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("phoxal API metadata section in {describe} is not valid JSON"))
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
    /// as the RESOLVED `TOPIC` (`"y2026_7/battery/state"` - `battery::State`
    /// lives on the standalone `y2026_7` generation, D1's ground-breaker),
    /// not the source-written `api::battery::State` (F2-names).
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
        assert_eq!(
            meta.contracts,
            vec![ParticipantMetaContract {
                field: "state".to_string(),
                role: "publish".to_string(),
                contract: "y2026_7/battery/state".to_string(),
            }]
        );
        Ok(())
    }

    /// A privileged tool defaults to `Api = ()` (`ParticipantKind::default_api`
    /// in `phoxal-macros/src/authoring.rs`), so it never derives
    /// `#[derive(phoxal::Api)]` and its binary carries no metadata section at
    /// all - a valid, expected shape (zero contracts), not an extraction
    /// error. Proven end-to-end against the real `phoxal-tool-joypad` binary.
    #[test]
    fn a_binary_with_no_section_parses_as_zero_contracts() -> Result<()> {
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
        let payload =
            br#"{"participant_api":"Api","contracts":[{"field":"state","role":"publish","contract":"y2026_1/drive/state"}]}"#;
        let expected = vec![ParticipantMetaContract {
            field: "state".to_string(),
            role: "publish".to_string(),
            contract: "y2026_1/drive/state".to_string(),
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

    /// A foreign-format object file with NO phoxal section still parses cleanly
    /// as zero contracts (the `Api = ()` shape), proving the None-vs-error split
    /// is not host-format-specific.
    #[test]
    fn foreign_object_without_section_is_zero_contracts() -> Result<()> {
        let elf = synthesize_object(
            object::BinaryFormat::Elf,
            object::Architecture::Aarch64,
            b".some_other_section",
            b"",
            b"unrelated",
        );
        let meta = extract_participant_metadata_from_bytes(&elf, "synthetic aarch64 ELF")?;
        assert!(meta.contracts.is_empty());
        Ok(())
    }
}
