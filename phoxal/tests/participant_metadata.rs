//! What a linked participant binary carries, proven on a real linked binary.
//!
//! **What a unit test cannot see: the linker.** `#[used]` keeps a static alive
//! against its own compilation unit's dead-code elimination, but ELF
//! `--gc-sections` still drops any section unreachable from `main` at final
//! link time, which is exactly what would strip every participant's
//! `.phoxal_meta` section if `Participant::__retain_embedded_metadata` stopped
//! anchoring it. A check that reads source cannot see that happen. The only
//! honest check builds a real participant and inspects the linked object file,
//! so that is what these tests do: they compile a throwaway participant against
//! the real in-tree `phoxal`/`phoxal-macros` crates in a standalone temp
//! workspace, then read the linked binary's metadata section back with the
//! `object` crate - never executing it, the same "read the bytes, don't run the
//! binary" discipline `phoxal-cli`'s reader follows.
//!
//! This proves the mechanism on whatever object format the test host produces.
//! ELF, which `.github/workflows/ci.yml`'s `ubuntu-latest` runner builds, is
//! the format `--gc-sections` drops sections on, so a passing CI run is the
//! real regression guard. Mach-O keeps the section either way, so a local macOS
//! run confirms the section and its contents without exercising the ELF path.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

/// The workspace this crate is part of: the facade sits directly at the
/// workspace root, so its manifest directory's parent is that root.
fn workspace_root() -> Result<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("the facade's manifest directory has no workspace parent")?
        .to_path_buf())
}

/// The two linker-section names a participant's embedded metadata can live
/// under (`crates/macros/src/authoring.rs`'s `link_section_attrs`):
/// `.phoxal_meta` on ELF, `__phoxal_meta` on Mach-O (`object`'s `ObjectSection`
/// name match ignores the `__DATA` segment qualifier). Duplicated here rather
/// than imported: no framework-side crate reads object files, and the only
/// other place this exact list lives is `phoxal-cli`'s
/// `participant_metadata.rs`, in a sibling repository this crate does not and
/// should not depend on.
const PARTICIPANT_META_SECTION_NAMES: [&str; 2] = [".phoxal_meta", "__phoxal_meta"];

#[test]
fn participant_metadata_section_survives_the_linker() -> Result<()> {
    let meta = linked_participant_metadata(
        "phoxal-elf-meta-probe",
        r#"use phoxal::prelude::*;

#[phoxal::service(id = "elf-meta-probe")]
struct ElfMetaProbe;

impl Participant for ElfMetaProbe {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<ElfMetaProbe>()
}
"#,
    )?;
    assert_eq!(
        meta.get("id").and_then(Value::as_str),
        Some("elf-meta-probe"),
        "unexpected participant metadata in the linked section: {meta:?}"
    );
    Ok(())
}

/// The root brain's complete process contract, proven on a real linked
/// binary rather than on macro tokens: `#[phoxal::brain]` embeds exactly
/// the fixed `brain` identity, the distinct `brain` kind, and the ordinary
/// unit-config schema every `Config = ()` role emits - no more fields and
/// no project-chosen identity. This is what `phoxal-cli` reads out of
/// `bin/brain` before launching it.
#[test]
fn a_brain_binary_embeds_the_exact_root_brain_metadata_record() -> Result<()> {
    // The probe package is deliberately NOT named `brain`: the identity is
    // fixed by the role attribute, never derived from the package name.
    let meta = linked_participant_metadata(
        "some-robot-project",
        r#"use phoxal::prelude::*;

#[phoxal::brain]
struct Brain;

impl Participant for Brain {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Brain>()
}
"#,
    )?;
    use phoxal::participant::metadata::ParticipantContractRecord;
    use phoxal::participant::metadata::ParticipantKind;
    use phoxal::participant::metadata::ParticipantMetadataRecord;
    use phoxal::version::FrameworkVersion;

    // Compared against the framework's own typed writer rather than a
    // hand-written JSON literal: the linked bytes and the record the
    // framework says it emits are the same document or this fails.
    assert_eq!(
        meta,
        serde_json::to_value(ParticipantMetadataRecord::V0 {
            contract: ParticipantContractRecord {
                framework: FrameworkVersion::CURRENT,
                id: "brain",
                kind: ParticipantKind::Brain,
                connection: None,
                config_schema: serde_json::json!({"type": "null"}),
            },
        })?,
        "unexpected root brain metadata in the linked section"
    );
    Ok(())
}

/// A driver that declared `connection = …` carries the kind in its embedded
/// record, alongside the schema of the config it declares. That pair is what
/// build tooling compares against one authored `driver:` block - one half per
/// owner - so it is proven on a real linked binary rather than on macro tokens.
#[test]
fn a_declared_driver_binary_embeds_its_connection_kind_and_config_schema() -> Result<()> {
    let meta = linked_participant_metadata(
        "phoxal-component-declared-probe",
        r#"use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct ProbeConfig {
    // The probe never runs; the field exists so the emitted schema has one.
    #[allow(dead_code)]
    id: u8,
}

#[phoxal::driver(id = "declared-probe", config = ProbeConfig, connection = serial)]
struct DeclaredProbe;

impl Participant for DeclaredProbe {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<DeclaredProbe>()
}
"#,
    )?;
    use phoxal::model::connection::ConnectionKind;
    use phoxal::participant::metadata::{ParticipantKind, ParticipantMetadata};

    let bytes = serde_json::to_vec(&meta)?;
    let metadata = ParticipantMetadata::from_bytes(&bytes)
        .context("the linked record must parse as the tagged v0 document")?;
    let contract = metadata.contract();
    assert_eq!(contract.kind, ParticipantKind::Driver);
    assert_eq!(contract.connection, Some(ConnectionKind::Serial));
    assert_eq!(
        contract.config_schema.pointer("/properties/id/type"),
        Some(&Value::from("integer")),
        "unexpected config schema in the linked section: {meta:?}"
    );
    Ok(())
}

/// A driver that declared no connection carries `null`: the same document with
/// the other value. "Accepts every kind" is a decision the driver made, not a
/// key that happens to be missing.
#[test]
fn an_undeclared_driver_binary_embeds_a_null_connection() -> Result<()> {
    let meta = linked_participant_metadata(
        "phoxal-component-undeclared-probe",
        r#"use phoxal::prelude::*;

#[phoxal::driver(id = "undeclared-probe")]
struct UndeclaredProbe;

impl Participant for UndeclaredProbe {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<UndeclaredProbe>()
}
"#,
    )?;
    assert_eq!(
        meta.as_object()
            .context("the embedded record must be a JSON object")?
            .get("connection"),
        Some(&Value::Null),
        "unexpected connection in the linked section: {meta:?}"
    );
    Ok(())
}

/// The embedded record is a document, not a struct with a schema field: it must
/// parse straight into the tagged `V0` variant. Its `framework` field is the
/// binary's whole compatibility claim: it carries the exact train version - the
/// provenance a validator reads the compatibility line from - and none of the
/// per-boundary identities that claim replaced.
#[test]
fn a_linked_participant_record_carries_the_framework_train_version_alone() -> Result<()> {
    let meta = linked_participant_metadata(
        "phoxal-tagged-meta-probe",
        r#"use phoxal::prelude::*;

#[phoxal::service(id = "tagged-meta-probe")]
struct TaggedMetaProbe;

impl Participant for TaggedMetaProbe {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<TaggedMetaProbe>()
}
"#,
    )?;
    use phoxal::__private::compatibility as compat;
    use phoxal::participant::metadata::{ParticipantKind, ParticipantMetadata};
    use phoxal::version::FrameworkVersion;

    let object = meta
        .as_object()
        .context("the embedded record must be a JSON object")?;
    assert_eq!(
        object.get("framework").and_then(Value::as_str),
        Some(compat::FRAMEWORK),
        "the embedded record must carry the exact framework train version: {meta:?}"
    );
    for absent in [
        "api",
        "schemas",
        "bus",
        "launch",
        "runtime",
        "version",
        // The topology declaration is deleted, not merely unset here.
        "requirement",
    ] {
        assert!(
            !object.contains_key(absent),
            "the framework train version is the whole compatibility claim, so '{absent}' must be \
             absent: {meta:?}"
        );
    }

    let bytes = serde_json::to_vec(&meta)?;
    let metadata = ParticipantMetadata::from_bytes(&bytes)
        .context("the linked record must parse as the tagged v0 document")?;
    let contract = metadata.contract();
    assert_eq!(contract.framework, FrameworkVersion::CURRENT);
    assert_eq!(contract.kind, ParticipantKind::Service);
    assert_ne!(contract.kind, ParticipantKind::Brain);
    assert_eq!(contract.id.as_str(), "tagged-meta-probe");
    Ok(())
}

/// Build `main_rs` as a standalone `package` binary against the in-tree
/// `phoxal` crate and read its embedded participant-metadata section back
/// out of the linked artifact. The binary is never executed.
fn linked_participant_metadata(package: &str, main_rs: &str) -> Result<Value> {
    use object::{Object, ObjectSection};

    let workspace_root = workspace_root()?;
    let phoxal_path = workspace_root.join("phoxal");

    let probe_dir = tempfile::tempdir().context("failed to create temp probe crate dir")?;
    let crate_dir = probe_dir.path();
    fs::create_dir_all(crate_dir.join("src"))?;
    fs::write(
        crate_dir.join("Cargo.toml"),
        format!(
            r#"[workspace]

[package]
name = "{package}"
version = "0.1.0"
edition = "2024"
publish = false

[[bin]]
name = "{package}"
path = "src/main.rs"

[dependencies]
phoxal = {{ path = {phoxal_path:?} }}
# A probe that declares a config derives `Deserialize` for it, exactly as a real
# participant crate does; `phoxal` re-exports no derive of its own.
serde = {{ version = "1", features = ["derive"] }}
"#,
            phoxal_path = phoxal_path.display().to_string(),
        ),
    )?;
    fs::write(crate_dir.join("src/main.rs"), main_rs)?;

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = std::process::Command::new(&cargo)
        .arg("build")
        .arg("--manifest-path")
        .arg(crate_dir.join("Cargo.toml"))
        .status()
        .context("failed to spawn cargo build for the probe participant")?;
    assert!(
        status.success(),
        "cargo build for the linker-section probe participant failed"
    );

    let binary_name = if cfg!(windows) {
        format!("{package}.exe")
    } else {
        package.to_string()
    };
    let binary_path = crate_dir.join("target").join("debug").join(binary_name);
    let data = fs::read(&binary_path).with_context(|| {
        format!(
            "failed to read the built probe binary at {}",
            binary_path.display()
        )
    })?;
    let file = object::File::parse(&*data).with_context(|| {
        format!(
            "{} is not a recognized object file (ELF/Mach-O/...)",
            binary_path.display()
        )
    })?;

    let mut section_bytes = None;
    for name in PARTICIPANT_META_SECTION_NAMES {
        if let Some(section) = file.section_by_name(name) {
            section_bytes = Some(section.data().with_context(|| {
                format!("failed to read section '{name}' data from the probe binary")
            })?);
            break;
        }
    }
    let section_bytes = section_bytes.with_context(|| {
        format!(
            "the built probe binary carries no participant metadata section ({}); the ELF \
             --gc-sections defeat in phoxal-macros' expand_participant/Participant::\
             __retain_embedded_metadata has regressed",
            PARTICIPANT_META_SECTION_NAMES.join(" or ")
        )
    })?;

    serde_json::from_slice(section_bytes)
        .context("the participant metadata section did not parse as JSON")
}
