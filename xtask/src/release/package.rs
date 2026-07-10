use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::release::metadata::{self, ParticipantMeta};
use crate::workspace::{
    OfficialArtifact, TARGET_INDEPENDENT_SCOPE, Workspace, require_nonempty_artifacts,
};

#[derive(Debug, clap::Args)]
pub struct Args {
    #[arg(
        value_name = "PACKAGE",
        required_unless_present = "all",
        conflicts_with = "all"
    )]
    pub package: Option<String>,
    #[arg(long)]
    pub all: bool,
    #[arg(long, value_name = "DIR", default_value = "target/xtask/release")]
    pub out: PathBuf,
    #[arg(long, value_name = "TRIPLE")]
    pub target: Option<String>,
    /// Validate target selection and print the planned build/package work
    /// without invoking cargo or writing release files.
    #[arg(long)]
    pub dry_run: bool,
}

/// A packaged artifact: a tarball plus its checksum. No `emit-apis` sidecar -
/// the catalog inlines contract/config metadata directly (`cargo xtask
/// catalog generate` runs `emit-apis` itself; see `xtask/src/catalog/generate.rs`).
#[derive(Clone, Debug)]
pub struct PackagedArtifact {
    pub tarball: PathBuf,
    pub checksum: PathBuf,
}

/// A previously packaged artifact's on-disk facts, read back from `package_dir`.
#[derive(Clone, Debug)]
pub(crate) struct PackagedOutput {
    pub tarball: PathBuf,
    pub checksum: PathBuf,
    pub tarball_name: String,
    pub checksum_name: String,
    pub tarball_sha256: String,
    /// The tarball's byte length - the catalog's `Blob.size` (design doc
    /// `organization/tmp/ci-release-refactor/design.md` §3).
    pub tarball_size: u64,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let selected = select_artifacts(&workspace, &args)?;
    require_nonempty_artifacts(&selected)?;
    let out_dir = workspace_relative_out_dir(&workspace, &args.out);
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create output directory {}", out_dir.display()))?;
    let host_triple = host_triple(workspace.root())?;
    let target_triple = args.target.clone().unwrap_or_else(|| host_triple.clone());

    for artifact in selected {
        validate_supported_target(&artifact, &target_triple)?;
        if args.dry_run {
            println!(
                "would package {} v{} for {} using {}",
                artifact.package,
                artifact.version,
                target_triple,
                if target_triple == TARGET_INDEPENDENT_SCOPE {
                    "a plain asset tarball"
                } else {
                    "cargo auditable build"
                }
            );
            continue;
        }
        let packaged = package_artifact(&workspace, &artifact, &out_dir, &target_triple)
            .with_context(|| format!("failed to package {}", artifact.package))?;
        println!(
            "packaged {} v{} for {} -> {}, {}",
            artifact.package,
            artifact.version,
            target_triple,
            packaged.tarball.display(),
            packaged.checksum.display(),
        );
    }

    Ok(())
}

pub(crate) fn workspace_relative_out_dir(workspace: &Workspace, out_dir: &Path) -> PathBuf {
    if out_dir.is_absolute() {
        out_dir.to_path_buf()
    } else {
        workspace.root().join(out_dir)
    }
}

fn select_artifacts(workspace: &Workspace, args: &Args) -> Result<Vec<OfficialArtifact>> {
    if args.all {
        return Ok(workspace.official_artifacts().to_vec());
    }

    let package_name = args
        .package
        .as_deref()
        .context("package is required unless --all is present")?;
    let artifact = workspace.official_artifact(package_name)?;
    Ok(vec![artifact.clone()])
}

pub(crate) fn package_artifact(
    workspace: &Workspace,
    artifact: &OfficialArtifact,
    out_dir: &Path,
    target_triple: &str,
) -> Result<PackagedArtifact> {
    validate_supported_target(artifact, target_triple)?;
    if target_triple == TARGET_INDEPENDENT_SCOPE {
        return package_component_assets(artifact, out_dir, target_triple);
    }

    artifact.require_package_name()?;
    build_target_artifact(workspace.root(), artifact, target_triple)?;
    let binary_path = target_binary_path(workspace, artifact, target_triple)?;
    // Packaging still validates the participant's compiled-in API metadata as
    // a fail-fast gate (a broken/absent `#[derive(phoxal::Api)]` section must
    // not reach the tarball stage), but no longer writes it anywhere: the
    // catalog inlines contract metadata by extracting the section itself at
    // generation time (`xtask/src/catalog/generate.rs`). Extraction reads the
    // object file directly - no execution, so (unlike the old `emit-apis`
    // subprocess call) it needs no separate host-runnable build even when
    // `target_triple` is cross-compiled.
    extract_and_validate_metadata(&binary_path, artifact)?;

    let stem = asset_stem(artifact, target_triple);
    let tarball = out_dir.join(format!("{stem}.tar.zst"));
    let checksum = out_dir.join(format!("{stem}.tar.zst.sha256"));

    write_tar_zst(&tarball, &binary_path, artifact.require_bin_name()?)?;
    write_sha256(&tarball, &checksum)?;

    Ok(PackagedArtifact { tarball, checksum })
}

/// The bundle files a [`ArtifactKind::Component`] crate's target-independent
/// asset output includes, relative to `artifact.crate_dir` (the flattened
/// `component/<id>/` crate directory, design doc §9): `component.yaml`,
/// `simulation.yaml`, `structure.urdf` when present, and the full `meshes/`
/// tree. This is an explicit allowlist, so the crate's own files (`Cargo.toml`,
/// `src/`, `CHANGELOG.md`) are naturally excluded without special-casing them.
const COMPONENT_ASSETS_TOP_LEVEL_FILES: [&str; 3] =
    ["component.yaml", "simulation.yaml", "structure.urdf"];
const COMPONENT_ASSETS_TREE_DIRS: [&str; 1] = ["meshes"];

/// Packages a component crate's target-independent asset bundle: no cargo
/// build, no binary, no `emit-apis` sidecar - just a deterministic tarball of
/// the component's asset files plus a checksum (docs #21, design doc §9).
fn package_component_assets(
    artifact: &OfficialArtifact,
    out_dir: &Path,
    target_triple: &str,
) -> Result<PackagedArtifact> {
    if target_triple != TARGET_INDEPENDENT_SCOPE {
        bail!(
            "{} asset bundle can only be packaged for the target-independent scope \
             '{TARGET_INDEPENDENT_SCOPE}', not '{target_triple}'",
            artifact.package
        );
    }

    let stem = asset_stem(artifact, target_triple);
    let tarball = out_dir.join(format!("{stem}.tar.zst"));
    let checksum = out_dir.join(format!("{stem}.tar.zst.sha256"));

    let entries = component_assets_bundle_entries(&artifact.crate_dir)?;
    write_component_assets_tar_zst(&tarball, &artifact.crate_dir, &entries)?;
    write_sha256(&tarball, &checksum)?;

    Ok(PackagedArtifact { tarball, checksum })
}

/// Walks `component_dir` (the `component/<id>/` crate directory) and returns
/// every bundle file's path relative to `component_dir`, sorted for a
/// deterministic tarball. Only the allowlisted asset files/dirs
/// ([`COMPONENT_ASSETS_TOP_LEVEL_FILES`], [`COMPONENT_ASSETS_TREE_DIRS`]) are
/// considered, so the crate's `Cargo.toml`/`src/` are never walked.
fn component_assets_bundle_entries(component_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();

    for name in COMPONENT_ASSETS_TOP_LEVEL_FILES {
        if component_dir.join(name).is_file() {
            entries.push(PathBuf::from(name));
        }
    }

    for dir_name in COMPONENT_ASSETS_TREE_DIRS {
        let dir = component_dir.join(dir_name);
        if dir.is_dir() {
            collect_files_sorted(&dir, Path::new(dir_name), &mut entries)?;
        }
    }

    entries.sort();
    Ok(entries)
}

/// Recursively collects every regular file under `dir` into `entries` as paths
/// relative to the bundle root (`relative_prefix` joined with the file's name
/// under `dir`), sorting each directory's children so the walk is
/// deterministic regardless of filesystem iteration order.
fn collect_files_sorted(
    dir: &Path,
    relative_prefix: &Path,
    entries: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut children = fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to read {}", dir.display()))?;
    children.sort();

    for child in children {
        let name = child
            .file_name()
            .with_context(|| format!("{} has no file name", child.display()))?;
        let relative = relative_prefix.join(name);
        if child.is_dir() {
            collect_files_sorted(&child, &relative, entries)?;
        } else if child.is_file() {
            entries.push(relative);
        }
    }

    Ok(())
}

/// Writes a deterministic tarball of `entries` (paths relative to
/// `component_dir`): sorted walk order (already guaranteed by the caller),
/// mode `0o644` (asset data, never executable), mtime 0 - mirroring the
/// reproducibility choices [`write_tar_zst`] makes for a binary artifact.
fn write_component_assets_tar_zst(
    tarball: &Path,
    component_dir: &Path,
    entries: &[PathBuf],
) -> Result<()> {
    let tarball_file = File::create(tarball)
        .with_context(|| format!("failed to create tarball {}", tarball.display()))?;
    let encoder = zstd::Encoder::new(tarball_file, 0)
        .with_context(|| format!("failed to start zstd encoder for {}", tarball.display()))?;
    let mut archive = tar::Builder::new(encoder);
    archive.follow_symlinks(false);

    for relative in entries {
        let source = component_dir.join(relative);
        let mut file =
            File::open(&source).with_context(|| format!("failed to open {}", source.display()))?;
        let size = file
            .metadata()
            .with_context(|| format!("failed to stat {}", source.display()))?
            .len();
        let mut header = tar::Header::new_gnu();
        header.set_size(size);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        archive
            .append_data(&mut header, relative, &mut file)
            .with_context(|| {
                format!(
                    "failed to append {} to {}",
                    relative.display(),
                    tarball.display()
                )
            })?;
    }

    let encoder = archive
        .into_inner()
        .with_context(|| format!("failed to finish tar archive {}", tarball.display()))?;
    encoder
        .finish()
        .with_context(|| format!("failed to finish zstd stream {}", tarball.display()))?;

    Ok(())
}

pub(crate) fn validate_supported_target(
    artifact: &OfficialArtifact,
    target_triple: &str,
) -> Result<()> {
    if artifact.supports_target(target_triple) {
        return Ok(());
    }
    bail!(
        "{} does not support target {}; supported targets: {}",
        artifact.package,
        target_triple,
        artifact.supported_target_triples().join(", ")
    )
}

fn build_target_artifact(
    root: &Path,
    artifact: &OfficialArtifact,
    target_triple: &str,
) -> Result<()> {
    let package_name = artifact.require_package_name()?;
    let mut command = Command::new("cargo");
    command
        .args([
            "auditable",
            "build",
            "-p",
            package_name,
            "--release",
            "--target",
            target_triple,
        ])
        .current_dir(root);

    run_cargo_build_command(
        command,
        &format!(
            "cargo auditable build for {} target {}",
            artifact.package, target_triple
        ),
    )
}

fn run_cargo_build_command(mut command: Command, label: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("failed to spawn {label}"))?;
    if !output.status.success() {
        bail!(
            "{label} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

fn target_binary_path(
    workspace: &Workspace,
    artifact: &OfficialArtifact,
    target_triple: &str,
) -> Result<PathBuf> {
    Ok(workspace
        .target_dir()
        .join(target_triple)
        .join("release")
        .join(format!(
            "{}{}",
            artifact.require_bin_name()?,
            exe_suffix_for_target(target_triple)
        )))
}

fn exe_suffix_for_target(target_triple: &str) -> &'static str {
    if target_triple.contains("windows") {
        ".exe"
    } else {
        ""
    }
}

/// Fail-fast schema check on extracted metadata: every embedded contract entry
/// must carry a non-empty generation, contract, and role.
fn validate_metadata(meta: &ParticipantMeta, artifact: &OfficialArtifact) -> Result<()> {
    for contract in &meta.contracts {
        if contract.generation.trim().is_empty() {
            bail!(
                "{} has a contract entry with an empty generation",
                artifact.package
            );
        }
        if contract.contract.trim().is_empty() {
            bail!(
                "{} has a contract entry with an empty contract name",
                artifact.package
            );
        }
        if contract.role.trim().is_empty() {
            bail!(
                "{} has a contract entry with an empty role",
                artifact.package
            );
        }
    }
    Ok(())
}

/// Runs `#[derive(phoxal::Api)]`'s fail-fast gate on a just-built binary: a
/// broken/absent metadata section must not reach the tarball stage. Reads the
/// object file directly - no execution of the artifact.
fn extract_and_validate_metadata(
    binary_path: &Path,
    artifact: &OfficialArtifact,
) -> Result<ParticipantMeta> {
    let meta = metadata::extract_participant_metadata(binary_path).with_context(|| {
        format!(
            "failed to extract API metadata for {} from {}",
            artifact.package,
            binary_path.display()
        )
    })?;
    validate_metadata(&meta, artifact)?;
    Ok(meta)
}

/// Extracts a participant's API metadata from the ACTUAL packaged binary being
/// released - the cross-compiled artifact inside its `{stem}.tar.zst` under
/// `package_dir`, not a fresh native rebuild. This is what makes the catalog's
/// `contracts[]` physically inseparable from the shipped bytes: on the
/// `catalog-publish` x86_64 host it reads the section straight out of an
/// aarch64 (or any-target) binary, thanks to the format/arch-agnostic reader
/// in [`crate::release::metadata`]. Picks the first `target_triples` entry
/// whose tarball is present (the embedded section is identical across targets -
/// contract identity is target-independent - so any one released binary is
/// authoritative).
pub(crate) fn extract_metadata_from_packaged(
    artifact: &OfficialArtifact,
    package_dir: &Path,
    target_triples: &[String],
) -> Result<ParticipantMeta> {
    let bin_name = artifact.require_bin_name()?;
    for triple in target_triples {
        let stem = asset_stem(artifact, triple);
        let tarball = package_dir.join(format!("{stem}.tar.zst"));
        if !tarball.is_file() {
            continue;
        }
        let object_bytes = read_binary_from_tarball(&tarball, bin_name)?;
        let describe = format!(
            "{} ({triple}, from {})",
            artifact.package,
            tarball.display()
        );
        let meta = metadata::extract_participant_metadata_from_bytes(&object_bytes, &describe)
            .with_context(|| format!("failed to extract API metadata for {}", artifact.package))?;
        validate_metadata(&meta, artifact)?;
        return Ok(meta);
    }
    bail!(
        "no packaged tarball found for {} in {} among targets [{}]",
        artifact.package,
        package_dir.display(),
        target_triples.join(", ")
    )
}

/// Reads the single binary named `bin_name` out of a `.tar.zst` release
/// tarball into memory (the archive holds exactly that one entry - see
/// [`write_tar_zst`]).
fn read_binary_from_tarball(tarball: &Path, bin_name: &str) -> Result<Vec<u8>> {
    let file = File::open(tarball)
        .with_context(|| format!("failed to open tarball {}", tarball.display()))?;
    let decoder = zstd::Decoder::new(file)
        .with_context(|| format!("failed to start zstd decoder for {}", tarball.display()))?;
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .with_context(|| format!("failed to read entries of {}", tarball.display()))?;
    for entry in entries {
        let mut entry =
            entry.with_context(|| format!("failed to read an entry of {}", tarball.display()))?;
        let path = entry
            .path()
            .with_context(|| format!("entry of {} has no path", tarball.display()))?;
        if path.as_os_str() == std::ffi::OsStr::new(bin_name) {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .with_context(|| format!("failed to read {bin_name} from {}", tarball.display()))?;
            return Ok(bytes);
        }
    }
    bail!("tarball {} does not contain {bin_name}", tarball.display())
}

/// Builds `artifact` for the host with a plain (non-`auditable`, non-release)
/// `cargo build` and extracts its compiled-in API metadata. Used by `catalog
/// generate --metadata-only` (CI's cheap PR gate): no tarball exists yet, so it
/// builds just enough to populate the participant's `#[derive(phoxal::Api)]`
/// linker section. Package-output mode reads the real released binary instead
/// (see [`extract_metadata_from_packaged`]).
pub(crate) fn build_and_extract_metadata(
    workspace: &Workspace,
    artifact: &OfficialArtifact,
) -> Result<ParticipantMeta> {
    let package_name = artifact.require_package_name()?;
    let mut command = Command::new("cargo");
    command
        .args(["build", "--quiet", "-p", package_name])
        .current_dir(workspace.root());
    run_cargo_build_command(command, &format!("cargo build for {}", artifact.package))?;

    let binary_path = workspace.target_dir().join("debug").join(format!(
        "{}{}",
        artifact.require_bin_name()?,
        std::env::consts::EXE_SUFFIX
    ));
    extract_and_validate_metadata(&binary_path, artifact)
}

/// Returns the local host triple used in asset names. This seed packages only
/// host builds; cross-target naming and build orchestration belongs to native
/// release CI plan #01.
pub(crate) fn host_triple(root: &Path) -> Result<String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .current_dir(root)
        .output()
        .context("failed to run rustc -vV")?;
    if !output.status.success() {
        bail!(
            "rustc -vV failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8(output.stdout).context("rustc -vV output was not UTF-8")?;
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_string)
        .context("rustc -vV output did not contain a host triple")
}

/// The release asset filename stem: a filesystem-safe projection of the
/// provider-qualified `package` (docs #21), not the Cargo crate name.
pub(crate) fn asset_stem(artifact: &OfficialArtifact, host_triple: &str) -> String {
    format!(
        "{}-v{}-{}",
        crate::workspace::filesystem_safe_package(&artifact.package),
        artifact.version,
        host_triple
    )
}

fn write_tar_zst(tarball: &Path, binary_path: &Path, bin_name: &str) -> Result<()> {
    let tarball_file = File::create(tarball)
        .with_context(|| format!("failed to create tarball {}", tarball.display()))?;
    let encoder = zstd::Encoder::new(tarball_file, 0)
        .with_context(|| format!("failed to start zstd encoder for {}", tarball.display()))?;
    let mut archive = tar::Builder::new(encoder);
    archive.follow_symlinks(false);

    let mut binary = File::open(binary_path)
        .with_context(|| format!("failed to open binary {}", binary_path.display()))?;
    let binary_size = binary
        .metadata()
        .with_context(|| format!("failed to stat binary {}", binary_path.display()))?
        .len();
    let mut header = tar::Header::new_gnu();
    header.set_size(binary_size);
    header.set_mode(0o755);
    header.set_mtime(0);
    header.set_cksum();
    archive
        .append_data(&mut header, bin_name, &mut binary)
        .with_context(|| format!("failed to append {bin_name} to {}", tarball.display()))?;
    let encoder = archive
        .into_inner()
        .with_context(|| format!("failed to finish tar archive {}", tarball.display()))?;
    encoder
        .finish()
        .with_context(|| format!("failed to finish zstd stream {}", tarball.display()))?;

    Ok(())
}

fn write_sha256(tarball: &Path, checksum_path: &Path) -> Result<()> {
    let digest = sha256_file(tarball)?;
    let filename = tarball
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("tarball path {} has no UTF-8 filename", tarball.display()))?;
    let mut checksum = File::create(checksum_path)
        .with_context(|| format!("failed to create checksum {}", checksum_path.display()))?;
    writeln!(checksum, "{digest}  {filename}")
        .with_context(|| format!("failed to write checksum {}", checksum_path.display()))?;
    Ok(())
}

pub(crate) fn read_packaged_output(
    artifact: &OfficialArtifact,
    package_dir: &Path,
    target_triple: &str,
) -> Result<PackagedOutput> {
    validate_supported_target(artifact, target_triple)?;
    let stem = asset_stem(artifact, target_triple);
    let tarball = package_dir.join(format!("{stem}.tar.zst"));
    let checksum = package_dir.join(format!("{stem}.tar.zst.sha256"));
    for path in [&tarball, &checksum] {
        if !path.is_file() {
            bail!(
                "missing packaged output for {} target {}: {}",
                artifact.package,
                target_triple,
                path.display()
            );
        }
    }

    let recorded = read_checksum_file(&checksum, &tarball)?;
    let computed = sha256_file(&tarball)?;
    if recorded != computed {
        bail!(
            "{} checksum file recorded {}, but computed {}",
            tarball.display(),
            recorded,
            computed
        );
    }
    let tarball_size = fs::metadata(&tarball)
        .with_context(|| format!("failed to stat {}", tarball.display()))?
        .len();

    Ok(PackagedOutput {
        tarball_name: file_name(&tarball)?,
        checksum_name: file_name(&checksum)?,
        tarball_sha256: computed,
        tarball_size,
        tarball,
        checksum,
    })
}

pub(crate) fn read_checksum_file(checksum: &Path, asset: &Path) -> Result<String> {
    let text = fs::read_to_string(checksum)
        .with_context(|| format!("failed to read {}", checksum.display()))?;
    let mut parts = text.split_whitespace();
    let digest = parts
        .next()
        .with_context(|| format!("{} is empty", checksum.display()))?;
    let filename = parts
        .next()
        .with_context(|| format!("{} is missing an asset filename", checksum.display()))?;
    let expected = asset
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no UTF-8 filename", asset.display()))?;
    if filename != expected {
        bail!(
            "{} names asset '{}', expected '{}'",
            checksum.display(),
            filename,
            expected
        );
    }
    Ok(digest.to_string())
}

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .with_context(|| format!("{} has no UTF-8 filename", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::ArtifactKind;

    #[test]
    fn asset_stem_uses_package_version_and_host_triple() {
        let artifact = OfficialArtifact {
            package: "phoxal/service-frame".to_string(),
            package_name: Some("phoxal-service-frame".to_string()),
            kind: ArtifactKind::Service,
            version: "0.19.1".to_string(),
            crate_dir: PathBuf::from("service/frame"),
            bin_name: Some("phoxal-service-frame".to_string()),
            id: "frame".to_string(),
            metadata: Default::default(),
        };

        assert_eq!(
            asset_stem(&artifact, "x86_64-unknown-linux-gnu"),
            "phoxal-service-frame-v0.19.1-x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn asset_stem_projects_component_package_to_filesystem_safe_form() {
        let artifact = OfficialArtifact {
            package: "phoxal/component-ddsm115".to_string(),
            package_name: Some("phoxal-component-ddsm115".to_string()),
            kind: ArtifactKind::Component,
            version: "0.1.5".to_string(),
            crate_dir: PathBuf::from("component/ddsm115"),
            bin_name: Some("phoxal-component-ddsm115".to_string()),
            id: "ddsm115".to_string(),
            metadata: Default::default(),
        };

        assert_eq!(
            asset_stem(&artifact, "aarch64-unknown-linux-gnu"),
            "phoxal-component-ddsm115-v0.1.5-aarch64-unknown-linux-gnu"
        );
    }

    /// Full package-output wiring proof (#2): build a real participant, tar it
    /// exactly as the release path does, then read its contracts back out of
    /// that `.tar.zst` via `extract_metadata_from_packaged` - i.e. from the
    /// actual shipped binary bytes, not a separate rebuild. (On the release CI
    /// host the same call reads a cross-compiled binary; the cross-FORMAT parse
    /// itself is proven hermetically in `metadata.rs`'s foreign-object tests.)
    #[test]
    fn extract_metadata_from_packaged_reads_the_tarball_binary() -> Result<()> {
        let workspace = Workspace::discover()?;
        let bin_name = "phoxal-service-battery";
        let status = Command::new("cargo")
            .args(["build", "--quiet", "-p", bin_name])
            .current_dir(workspace.root())
            .status()
            .context("failed to spawn cargo build for phoxal-service-battery")?;
        assert!(status.success(), "cargo build -p {bin_name} failed");
        let binary = workspace
            .target_dir()
            .join("debug")
            .join(format!("{bin_name}{}", std::env::consts::EXE_SUFFIX));

        let artifact = OfficialArtifact {
            package: "phoxal/service-battery".to_string(),
            package_name: Some(bin_name.to_string()),
            kind: ArtifactKind::Service,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::from("service/battery"),
            bin_name: Some(bin_name.to_string()),
            id: "battery".to_string(),
            metadata: Default::default(),
        };

        let dir = tempfile::tempdir().context("create tempdir")?;
        let triple = "some-target-triple";
        let stem = asset_stem(&artifact, triple);
        let tarball = dir.path().join(format!("{stem}.tar.zst"));
        write_tar_zst(&tarball, &binary, bin_name)?;

        let meta = extract_metadata_from_packaged(&artifact, dir.path(), &[triple.to_string()])?;
        assert_eq!(meta.contracts.len(), 1);
        assert_eq!(meta.contracts[0].role, "publish");
        assert_eq!(meta.contracts[0].generation, "y2026_7");
        assert_eq!(meta.contracts[0].contract, "battery::State");
        assert!(!meta.contracts[0].external);
        Ok(())
    }

    #[test]
    fn extract_metadata_from_packaged_fails_when_no_tarball_present() -> Result<()> {
        let artifact = OfficialArtifact {
            package: "phoxal/service-battery".to_string(),
            package_name: Some("phoxal-service-battery".to_string()),
            kind: ArtifactKind::Service,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::from("service/battery"),
            bin_name: Some("phoxal-service-battery".to_string()),
            id: "battery".to_string(),
            metadata: Default::default(),
        };
        let dir = tempfile::tempdir().context("create tempdir")?;
        let err = extract_metadata_from_packaged(
            &artifact,
            dir.path(),
            &["some-target-triple".to_string()],
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("no packaged tarball found"),
            "{err}"
        );
        Ok(())
    }
}
