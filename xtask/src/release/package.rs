use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::workspace::{
    ArtifactKind, OfficialArtifact, TARGET_INDEPENDENT_SCOPE, Workspace, require_nonempty_artifacts,
};

const BUS_ABI: &str = "phoxal-bus/v0";
const EMIT_SCHEMA: &str = "phoxal.emit-apis/v0";

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
                if artifact.kind.has_crate() {
                    "cargo auditable build"
                } else {
                    "a plain asset tarball"
                }
            );
            continue;
        }
        let packaged = package_artifact(
            &workspace,
            &artifact,
            &out_dir,
            &host_triple,
            &target_triple,
        )
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

pub(crate) fn package_artifacts(
    workspace: &Workspace,
    artifacts: &[OfficialArtifact],
    out_dir: &Path,
    host_triple: &str,
    target_triple: &str,
) -> Result<BTreeMap<String, PackagedArtifact>> {
    let mut packaged = BTreeMap::new();
    for artifact in artifacts {
        let output = package_artifact(workspace, artifact, out_dir, host_triple, target_triple)
            .with_context(|| format!("failed to package {}", artifact.package))?;
        packaged.insert(artifact.package.clone(), output);
    }
    Ok(packaged)
}

pub(crate) fn package_artifact(
    workspace: &Workspace,
    artifact: &OfficialArtifact,
    out_dir: &Path,
    host_triple: &str,
    target_triple: &str,
) -> Result<PackagedArtifact> {
    if !artifact.kind.has_crate() {
        return package_component_assets(artifact, out_dir, target_triple);
    }

    artifact.require_package_name()?;
    validate_supported_target(artifact, target_triple)?;
    build_target_artifact(workspace.root(), artifact, target_triple)?;
    let binary_path = target_binary_path(workspace, artifact, target_triple)?;
    let emit_binary_path = if target_triple == host_triple {
        binary_path.clone()
    } else {
        build_host_artifact(workspace.root(), artifact)?;
        host_binary_path(workspace, artifact)?
    };
    // Packaging still runs and validates `emit-apis` as a fail-fast gate (a
    // broken participant must not reach the tarball stage), but no longer
    // writes it anywhere: the catalog inlines contract/config metadata by
    // running `emit-apis` itself at generation time (`xtask/src/catalog/generate.rs`),
    // so there is no sidecar file for a consumer to fetch (killing the old
    // "double emit-apis": a sidecar per packaged target plus a second copy
    // implied by the catalog).
    let emit_stdout = run_emit_apis(&emit_binary_path)?;
    parse_emit_apis_json(&emit_stdout, &artifact.id, artifact.kind)?;

    let stem = asset_stem(artifact, target_triple);
    let tarball = out_dir.join(format!("{stem}.tar.zst"));
    let checksum = out_dir.join(format!("{stem}.tar.zst.sha256"));

    write_tar_zst(&tarball, &binary_path, artifact.require_bin_name()?)?;
    write_sha256(&tarball, &checksum)?;

    Ok(PackagedArtifact { tarball, checksum })
}

/// The bundle files an [`ArtifactKind::ComponentAssets`] tarball includes,
/// relative to `artifact.crate_dir` (the `component/<id>/` directory):
/// `component.yaml`, `simulation.yaml`, `structure.urdf` when present, and the
/// full `meshes/` tree. `driver/` (the separate `ComponentDriver` package) and
/// `assets.version` (xtask-internal release metadata, not a runtime asset) are
/// always excluded (docs #21).
const COMPONENT_ASSETS_TOP_LEVEL_FILES: [&str; 3] =
    ["component.yaml", "simulation.yaml", "structure.urdf"];
const COMPONENT_ASSETS_TREE_DIRS: [&str; 1] = ["meshes"];

/// Packages a target-independent `ComponentAssets` bundle: no cargo build, no
/// binary, no `emit-apis` sidecar - just a deterministic tarball of the
/// component's asset files plus a checksum (docs #21).
fn package_component_assets(
    artifact: &OfficialArtifact,
    out_dir: &Path,
    target_triple: &str,
) -> Result<PackagedArtifact> {
    validate_supported_target(artifact, target_triple)?;
    if target_triple != TARGET_INDEPENDENT_SCOPE {
        bail!(
            "{} is a component_assets package and can only be packaged for the \
             target-independent scope '{TARGET_INDEPENDENT_SCOPE}', not '{target_triple}'",
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

/// Walks `component_dir` (the `component/<id>/` directory) and returns every
/// bundle file's path relative to `component_dir`, sorted for a deterministic
/// tarball. Excludes `driver/` and [`crate::workspace::ASSETS_VERSION_FILE`].
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

fn build_host_artifact(root: &Path, artifact: &OfficialArtifact) -> Result<()> {
    let package_name = artifact.require_package_name()?;
    let mut command = Command::new("cargo");
    command
        .args(["build", "-p", package_name, "--release"])
        .current_dir(root);

    run_cargo_build_command(
        command,
        &format!("host cargo build for {}", artifact.package),
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

fn host_binary_path(workspace: &Workspace, artifact: &OfficialArtifact) -> Result<PathBuf> {
    Ok(workspace.target_dir().join("release").join(format!(
        "{}{}",
        artifact.require_bin_name()?,
        std::env::consts::EXE_SUFFIX
    )))
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

fn run_emit_apis(binary_path: &Path) -> Result<Vec<u8>> {
    let output = Command::new(binary_path)
        .arg("emit-apis")
        .output()
        .with_context(|| format!("failed to spawn {} emit-apis", binary_path.display()))?;
    if !output.status.success() {
        bail!(
            "{} emit-apis failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            binary_path.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if output.stdout.is_empty() {
        bail!(
            "{} emit-apis produced empty stdout\nstatus: {}\nstderr:\n{}",
            binary_path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(output.stdout)
}

pub(crate) fn emit_apis_from_cargo_run(
    root: &Path,
    artifact: &OfficialArtifact,
) -> Result<Vec<u8>> {
    let package_name = artifact.require_package_name()?;
    let output = Command::new("cargo")
        .args(["run", "--quiet", "-p", package_name, "--", "emit-apis"])
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to spawn cargo run for {}", artifact.package))?;
    if !output.status.success() {
        bail!(
            "cargo run for {} emit-apis failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            artifact.package,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if output.stdout.is_empty() {
        bail!(
            "{} emit-apis produced empty stdout\nstatus: {}\nstderr:\n{}",
            artifact.package,
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(output.stdout)
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

    Ok(PackagedOutput {
        tarball_name: file_name(&tarball)?,
        checksum_name: file_name(&checksum)?,
        tarball_sha256: computed,
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
fn validate_emit_apis_json(
    stdout: &[u8],
    expected_id: &str,
    expected_kind: ArtifactKind,
) -> Result<()> {
    parse_emit_apis_json(stdout, expected_id, expected_kind).map(|_| ())
}

pub(crate) fn parse_emit_apis_json(
    stdout: &[u8],
    expected_id: &str,
    expected_kind: ArtifactKind,
) -> Result<EmitApisMetadata> {
    let metadata: EmitApisMetadata =
        serde_json::from_slice(stdout).context("emit-apis stdout was not valid JSON")?;
    if metadata.schema != EMIT_SCHEMA {
        bail!(
            "emit-apis schema '{}' did not match expected '{}'",
            metadata.schema,
            EMIT_SCHEMA
        );
    }
    if metadata.artifact.id != expected_id {
        bail!(
            "emit-apis artifact.id '{}' did not match expected '{}'",
            metadata.artifact.id,
            expected_id
        );
    }
    let expected_kind_name = expected_kind.emit_apis_kind();
    if metadata.artifact.kind != expected_kind_name {
        bail!(
            "emit-apis artifact.kind '{}' did not match expected '{}'",
            metadata.artifact.kind,
            expected_kind_name
        );
    }
    match metadata.participant_class.as_deref() {
        Some("checked" | "privileged") => {}
        Some(value) => {
            bail!("emit-apis participant_class '{value}' must be 'checked' or 'privileged'")
        }
        None => bail!("emit-apis participant_class is missing"),
    }
    if metadata.api_version.trim().is_empty() {
        bail!("emit-apis api_version must not be empty");
    }
    if metadata.bus_abi != BUS_ABI {
        bail!(
            "emit-apis bus_abi '{}' did not match expected '{}'",
            metadata.bus_abi,
            BUS_ABI
        );
    }
    if metadata.required_contracts.is_empty()
        && !(expected_kind == ArtifactKind::Tool
            && metadata.participant_class.as_deref() == Some("privileged"))
    {
        bail!("emit-apis required_contracts must not be empty");
    }
    for (index, contract) in metadata.required_contracts.iter().enumerate() {
        let api_version = contract.api_version.as_deref().with_context(|| {
            format!("emit-apis required_contracts[{index}] is missing api_version")
        })?;
        if api_version != metadata.api_version {
            bail!(
                "emit-apis required_contracts[{index}].api_version '{}' did not match artifact api_version '{}'",
                api_version,
                metadata.api_version
            );
        }
        let schema_id = contract.schema_id.as_deref().with_context(|| {
            format!("emit-apis required_contracts[{index}] is missing schema_id")
        })?;
        if !is_schema_id(schema_id) {
            bail!(
                "emit-apis required_contracts[{index}].schema_id '{}' is not 16 lowercase hex characters",
                schema_id
            );
        }
        let family = contract
            .family
            .as_deref()
            .with_context(|| format!("emit-apis required_contracts[{index}] is missing family"))?;
        if family.trim().is_empty() {
            bail!("emit-apis required_contracts[{index}].family must not be empty");
        }
    }

    Ok(metadata)
}

fn is_schema_id(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct EmitApisMetadata {
    pub schema: String,
    pub artifact: Artifact,
    pub framework: Framework,
    pub api_version: String,
    pub participant_class: Option<String>,
    pub bus_abi: String,
    pub required_contracts: Vec<Contract>,
    /// The participant's `robot.yaml` config JSON Schema, inlined into the
    /// catalog's `ArtifactEntry::config_schema` (docs: catalog rewrite).
    pub config_schema: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct Artifact {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct Framework {
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct Contract {
    pub api_version: Option<String>,
    pub schema_id: Option<String>,
    pub family: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_emit() -> String {
        r#"{
  "schema": "phoxal.emit-apis/v0",
  "artifact": { "kind": "service", "id": "frame" },
  "framework": { "version": "0.21.0" },
  "api_version": "y2026_1",
  "participant_class": "checked",
  "bus_abi": "phoxal-bus/v0",
  "required_contracts": [
    {
      "api_version": "y2026_1",
      "schema_id": "0123456789abcdef",
      "family": "frame::LookupRequest"
    }
  ],
  "config_schema": { "type": "object" }
}"#
        .to_string()
    }

    fn validate(json: &str) -> Result<()> {
        validate_emit_apis_json(json.as_bytes(), "frame", ArtifactKind::Service)
    }

    fn assert_error_contains(error: &anyhow::Error, needle: &str) {
        assert!(
            error
                .chain()
                .any(|cause| cause.to_string().contains(needle)),
            "expected error chain to contain {needle:?}, got {error:?}"
        );
    }

    #[test]
    fn validation_accepts_real_shaped_emit_json() {
        validate(&valid_emit()).expect("valid emit JSON should pass");
    }

    #[test]
    fn validation_rejects_wrong_kind() {
        let json = valid_emit().replace(r#""kind": "service""#, r#""kind": "driver""#);
        let err = validate(&json).expect_err("wrong kind should fail");
        assert_error_contains(&err, "artifact.kind");
    }

    #[test]
    fn validation_rejects_wrong_schema_marker() {
        let json = valid_emit().replace("phoxal.emit-apis/v0", "phoxal.emit-apis/v1");
        let err = validate(&json).expect_err("wrong schema marker should fail");
        assert_error_contains(&err, "schema");
    }

    #[test]
    fn validation_rejects_missing_schema_marker() {
        let json = valid_emit().replace("  \"schema\": \"phoxal.emit-apis/v0\",\n", "");
        let err = validate(&json).expect_err("missing schema marker should fail");
        assert_error_contains(&err, "schema");
    }

    #[test]
    fn validation_rejects_missing_participant_class() {
        let json = valid_emit().replace("  \"participant_class\": \"checked\",\n", "");
        let err = validate(&json).expect_err("missing participant_class should fail");
        assert_error_contains(&err, "participant_class");
    }

    #[test]
    fn validation_rejects_bad_schema_id() {
        let json = valid_emit().replace("0123456789abcdef", "0123456789ABCDEF");
        let err = validate(&json).expect_err("bad schema_id should fail");
        assert_error_contains(&err, "schema_id");
    }

    #[test]
    fn validation_rejects_wrong_bus_abi() {
        let json = valid_emit().replace("phoxal-bus/v0", "phoxal-bus/v1");
        let err = validate(&json).expect_err("wrong bus_abi should fail");
        assert_error_contains(&err, "bus_abi");
    }

    #[test]
    fn validation_rejects_missing_required_contracts() {
        let json = valid_emit().replace("required_contracts", "renamed_contracts");
        let err = validate(&json).expect_err("missing required_contracts should fail");
        assert_error_contains(&err, "required_contracts");
    }

    #[test]
    fn validation_rejects_empty_required_contracts() {
        let json = valid_emit().replace(
            r#"  "required_contracts": [
    {
      "api_version": "y2026_1",
      "schema_id": "0123456789abcdef",
      "family": "frame::LookupRequest"
    }
  ],"#,
            r#"  "required_contracts": [],"#,
        );
        let err = validate(&json).expect_err("empty required_contracts should fail");
        assert_error_contains(&err, "required_contracts");
    }

    #[test]
    fn validation_accepts_empty_required_contracts_for_privileged_tool() {
        let json = valid_emit()
            .replace(r#""kind": "service""#, r#""kind": "tool""#)
            .replace(r#""id": "frame""#, r#""id": "router""#)
            .replace(
                r#""participant_class": "checked""#,
                r#""participant_class": "privileged""#,
            )
            .replace(
                r#"  "required_contracts": [
    {
      "api_version": "y2026_1",
      "schema_id": "0123456789abcdef",
      "family": "frame::LookupRequest"
    }
  ],"#,
                r#"  "required_contracts": [],"#,
            );

        validate_emit_apis_json(json.as_bytes(), "router", ArtifactKind::Tool)
            .expect("privileged tool with no graph contracts should validate");
    }

    #[test]
    fn validation_rejects_missing_contract_family() {
        let json = valid_emit().replace(
            "      \"schema_id\": \"0123456789abcdef\",\n      \"family\": \"frame::LookupRequest\"\n",
            "      \"schema_id\": \"0123456789abcdef\"\n",
        );
        let err = validate(&json).expect_err("missing contract family should fail");
        assert_error_contains(&err, "family");
    }

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
    fn asset_stem_projects_component_driver_package_to_filesystem_safe_form() {
        let artifact = OfficialArtifact {
            package: "phoxal/component-ddsm115-driver".to_string(),
            package_name: Some("phoxal-component-ddsm115-driver".to_string()),
            kind: ArtifactKind::ComponentDriver,
            version: "0.1.5".to_string(),
            crate_dir: PathBuf::from("component/ddsm115/driver"),
            bin_name: Some("phoxal-component-ddsm115-driver".to_string()),
            id: "ddsm115".to_string(),
            metadata: Default::default(),
        };

        assert_eq!(
            asset_stem(&artifact, "aarch64-unknown-linux-gnu"),
            "phoxal-component-ddsm115-driver-v0.1.5-aarch64-unknown-linux-gnu"
        );
    }
}
