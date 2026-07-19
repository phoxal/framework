use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::release::metadata::{self, ParticipantMeta};
use crate::workspace::{ASSETS_SCOPE, OfficialArtifact, Workspace, require_nonempty_artifacts};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// One or more package names to build and package together (one
    /// aggregate `cargo auditable build`, then packaged individually). Pass
    /// `--all` instead to select every official binary artifact.
    #[arg(value_name = "PACKAGE", conflicts_with = "all")]
    pub packages: Vec<String>,
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

/// A packaged artifact: a tarball plus its checksum. No metadata sidecar - the
/// participant metadata section remains embedded in the packaged binary.
#[derive(Clone, Debug)]
pub struct PackagedArtifact {
    pub tarball: PathBuf,
    pub checksum: PathBuf,
}

/// A previously packaged artifact's on-disk facts, read back from
/// `package_dir` - exactly what the catalog needs to form a `Blob` (design
/// doc `organization/tmp/ci-release-refactor/design.md` §3). The tarball/
/// checksum paths themselves aren't carried: the only reader
/// ([`crate::catalog::generate`]) forms its release URL from `tarball_name`,
/// never re-opens the file on disk.
#[derive(Clone, Debug)]
pub(crate) struct PackagedOutput {
    pub tarball_name: String,
    pub tarball_sha256: String,
    /// The tarball's byte length - the catalog's `Blob.size`.
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

    if target_triple == ASSETS_SCOPE {
        bail!(
            "'{ASSETS_SCOPE}' is not a valid --target for `release package` \
             (it packages binaries only); use `release assets` for a component's \
             asset bundle"
        );
    }
    for artifact in &selected {
        validate_supported_target(artifact, &target_triple)?;
    }

    if args.dry_run {
        for artifact in &selected {
            println!(
                "would package {} v{} for {} using cargo auditable build",
                artifact.package, artifact.version, target_triple,
            );
        }
        return Ok(());
    }

    let package_names = selected
        .iter()
        .map(|artifact| artifact.require_package_name())
        .collect::<Result<Vec<_>>>()?;
    build_target_artifacts(workspace.root(), &package_names, &target_triple).with_context(
        || {
            format!(
                "failed to build {} for target {target_triple}",
                package_names.join(", ")
            )
        },
    )?;

    let mut failures = Vec::new();
    for artifact in &selected {
        match package_artifact(&workspace, artifact, &out_dir, &target_triple) {
            Ok(packaged) => println!(
                "packaged {} v{} for {} -> {}, {}",
                artifact.package,
                artifact.version,
                target_triple,
                packaged.tarball.display(),
                packaged.checksum.display(),
            ),
            Err(err) => failures.push(format!("{}: {err:#}", artifact.package)),
        }
    }
    if !failures.is_empty() {
        bail!(
            "failed to package {} of {} artifact(s):\n{}",
            failures.len(),
            selected.len(),
            failures.join("\n")
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
    if args.packages.is_empty() {
        bail!("at least one PACKAGE is required, or pass --all");
    }

    args.packages
        .iter()
        .map(|package_name| workspace.official_artifact(package_name).cloned())
        .collect()
}

/// Packages `artifact`'s already-built `target_triple` binary (built by an
/// earlier, aggregate [`build_target_artifacts`] call - this function never
/// invokes cargo itself).
pub(crate) fn package_artifact(
    workspace: &Workspace,
    artifact: &OfficialArtifact,
    out_dir: &Path,
    target_triple: &str,
) -> Result<PackagedArtifact> {
    validate_supported_target(artifact, target_triple)?;
    artifact.require_package_name()?;
    let binary_path = target_binary_path(workspace, artifact, target_triple)?;
    // Participant packages must carry valid API metadata; infrastructure must
    // carry none. Both sides are fail-closed so adding a new non-participant
    // kind cannot silently weaken coherence coverage.
    if artifact.kind.embeds_participant_metadata() {
        extract_and_validate_metadata(&binary_path, artifact)?;
    } else {
        validate_metadata_absent(&binary_path, artifact)?;
    }

    let stem = asset_stem(artifact, target_triple);
    let tarball = out_dir.join(format!("{stem}.tar.zst"));
    let checksum = out_dir.join(format!("{stem}.tar.zst.sha256"));

    write_tar_zst(&tarball, &binary_path, artifact.require_bin_name()?)?;
    write_sha256(&tarball, &checksum)?;

    Ok(PackagedArtifact { tarball, checksum })
}

/// The bundle files a [`ArtifactKind::Component`] crate's asset output includes,
/// relative to `artifact.crate_dir` (the flattened `component/<id>/` crate
/// directory, design doc §9): `component.yaml`, `simulation.yaml`,
/// `structure.urdf` when present, and the full `meshes/` tree. This is an
/// explicit allowlist, so the crate's own files (`Cargo.toml`, `src/`,
/// `CHANGELOG.md`) are naturally excluded without special-casing them.
const COMPONENT_ASSETS_TOP_LEVEL_FILES: [&str; 3] =
    ["component.yaml", "simulation.yaml", "structure.urdf"];
const COMPONENT_ASSETS_TREE_DIRS: [&str; 1] = ["meshes"];

/// Packages a component crate's asset bundle: no cargo build, no binary, no
/// `emit-apis` sidecar - just a deterministic tarball of the component's asset
/// files plus a checksum (docs #21, design doc §9).
/// The `release assets` verb's implementation; `artifact` must be a
/// [`ArtifactKind::Component`] that [`ArtifactKind::ships_assets`].
pub(crate) fn package_component_asset_bundle(
    artifact: &OfficialArtifact,
    out_dir: &Path,
) -> Result<PackagedArtifact> {
    if !artifact.kind.ships_assets() {
        bail!(
            "{} is a {} package; only a Component package ships an asset bundle",
            artifact.package,
            artifact.kind
        );
    }

    let stem = asset_stem(artifact, ASSETS_SCOPE);
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

fn build_target_artifacts(root: &Path, package_names: &[&str], target_triple: &str) -> Result<()> {
    build_target_package_group(root, package_names, target_triple)
}

fn build_target_package_group(
    root: &Path,
    package_names: &[&str],
    target_triple: &str,
) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .args(["auditable", "build", "--release", "--target", target_triple])
        .current_dir(root);
    for package_name in package_names {
        command.args(["-p", package_name]);
    }

    run_cargo_build_command(
        command,
        &format!(
            "cargo auditable build for {} target {target_triple}",
            package_names.join(", ")
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
/// must carry a non-empty version, contract, and role.
fn validate_metadata(meta: &ParticipantMeta, artifact: &OfficialArtifact) -> Result<()> {
    for contract in &meta.contracts {
        if contract.version.trim().is_empty() {
            bail!(
                "{} has a contract entry with an empty version",
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

fn validate_metadata_absent(binary_path: &Path, artifact: &OfficialArtifact) -> Result<()> {
    let bytes = fs::read(binary_path)
        .with_context(|| format!("failed to read binary {}", binary_path.display()))?;
    let describe = format!("{} ({})", artifact.package, binary_path.display());
    let section = metadata::extract_participant_metadata_section_from_bytes(&bytes, &describe)
        .with_context(|| {
            format!(
                "failed to inspect API metadata for {} from {}",
                artifact.package,
                binary_path.display()
            )
        })?;
    if section.is_some() {
        bail!(
            "{} is an {} artifact and must not embed participant API metadata",
            artifact.package,
            artifact.kind
        );
    }
    Ok(())
}

/// Extracts a participant's API metadata from every packaged target binary
/// being released and enforces that the raw embedded metadata section is
/// byte-identical across them. This makes any target's section valid evidence
/// for the whole `(package, version)` and lets publish-time coherence operate
/// on the exact bytes users download without a fresh native rebuild.
pub(crate) fn extract_metadata_from_packaged(
    artifact: &OfficialArtifact,
    package_dir: &Path,
    target_triples: &[String],
) -> Result<ParticipantMeta> {
    if !artifact.kind.embeds_participant_metadata() {
        bail!(
            "{} is {} and does not have participant API metadata",
            artifact.package,
            artifact.kind
        );
    }
    let bin_name = artifact.require_bin_name()?;
    let mut canonical: Option<(String, Option<Vec<u8>>, ParticipantMeta)> = None;

    for triple in target_triples {
        let stem = asset_stem(artifact, triple);
        let tarball = package_dir.join(format!("{stem}.tar.zst"));
        if !tarball.is_file() {
            bail!(
                "missing packaged binary for {} v{} target {}: {}",
                artifact.package,
                artifact.version,
                triple,
                tarball.display()
            );
        }
        let object_bytes = read_binary_from_tarball(&tarball, bin_name)?;
        let describe = format!(
            "{} ({triple}, from {})",
            artifact.package,
            tarball.display()
        );
        let section =
            metadata::extract_participant_metadata_section_from_bytes(&object_bytes, &describe)
                .with_context(|| {
                    format!("failed to extract API metadata for {}", artifact.package)
                })?;
        let meta = metadata::parse_participant_metadata_section(section.as_deref(), &describe)?;
        validate_metadata(&meta, artifact)?;

        if let Some((canonical_target, canonical_section, _)) = &canonical {
            ensure_metadata_sections_equal(
                artifact,
                canonical_target,
                canonical_section.as_deref(),
                triple,
                section.as_deref(),
            )?;
        } else {
            canonical = Some((triple.clone(), section, meta));
        }
    }

    canonical.map(|(_, _, meta)| meta).with_context(|| {
        format!(
            "no binary targets were provided for {} v{}",
            artifact.package, artifact.version
        )
    })
}

/// Verifies that every packaged target for a non-participant artifact omits
/// the participant metadata section.
pub(crate) fn validate_no_metadata_from_packaged(
    artifact: &OfficialArtifact,
    package_dir: &Path,
    target_triples: &[String],
) -> Result<()> {
    if artifact.kind.embeds_participant_metadata() {
        bail!(
            "{} is a participant and requires metadata validation",
            artifact.package
        );
    }
    let bin_name = artifact.require_bin_name()?;
    if target_triples.is_empty() {
        bail!(
            "no binary targets were provided for {} v{}",
            artifact.package,
            artifact.version
        );
    }
    for triple in target_triples {
        let stem = asset_stem(artifact, triple);
        let tarball = package_dir.join(format!("{stem}.tar.zst"));
        if !tarball.is_file() {
            bail!(
                "missing packaged binary for {} v{} target {}: {}",
                artifact.package,
                artifact.version,
                triple,
                tarball.display()
            );
        }
        let object_bytes = read_binary_from_tarball(&tarball, bin_name)?;
        let describe = format!(
            "{} ({triple}, from {})",
            artifact.package,
            tarball.display()
        );
        let section =
            metadata::extract_participant_metadata_section_from_bytes(&object_bytes, &describe)
                .with_context(|| {
                    format!("failed to inspect API metadata for {}", artifact.package)
                })?;
        if section.is_some() {
            bail!(
                "{} target {} must not embed participant API metadata",
                artifact.package,
                triple
            );
        }
    }
    Ok(())
}

fn ensure_metadata_sections_equal(
    artifact: &OfficialArtifact,
    canonical_target: &str,
    canonical_section: Option<&[u8]>,
    candidate_target: &str,
    candidate_section: Option<&[u8]>,
) -> Result<()> {
    if canonical_section != candidate_section {
        bail!(
            "cross-target metadata mismatch for {} v{}: embedded metadata section differs between \
             targets {} and {}",
            artifact.package,
            artifact.version,
            canonical_target,
            candidate_target
        );
    }
    Ok(())
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
/// `cargo build` and extracts its compiled-in API metadata. Used by the
/// release-PR coherence gate, which checks the whole workspace before release.
pub(crate) fn build_and_extract_metadata(
    workspace: &Workspace,
    artifact: &OfficialArtifact,
) -> Result<ParticipantMeta> {
    if !artifact.kind.embeds_participant_metadata() {
        bail!(
            "{} is {} and does not participate in API coherence",
            artifact.package,
            artifact.kind
        );
    }
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
/// provider-qualified `package` (docs #21), not the Cargo crate name. A
/// asset bundle ([`ASSETS_SCOPE`]) carries no trailing scope token - it is the
/// one release output for its
/// `(package, version)` with no sibling to disambiguate from by filename, and
/// the packaged-output reader ([`crate::catalog::generate`]) classifies by
/// the release plan's `target_triples`, never the filename. A real target
/// triple keeps the `-{triple}` suffix so distinct architectures don't
/// collide in the same directory.
pub(crate) fn asset_stem(artifact: &OfficialArtifact, host_triple: &str) -> String {
    let package = crate::workspace::filesystem_safe_package(&artifact.package);
    let version = &artifact.version;
    if host_triple == ASSETS_SCOPE {
        format!("{package}-v{version}")
    } else {
        format!("{package}-v{version}-{host_triple}")
    }
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
        tarball_sha256: computed,
        tarball_size,
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

    fn infrastructure_router_artifact() -> OfficialArtifact {
        OfficialArtifact {
            package: "phoxal/infrastructure-router".to_string(),
            package_name: Some("phoxal-infrastructure-router".to_string()),
            kind: ArtifactKind::Infrastructure,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::from("infrastructure/router"),
            bin_name: Some("phoxal-infrastructure-router".to_string()),
            id: "router".to_string(),
            metadata: Default::default(),
        }
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

    #[test]
    fn asset_stem_drops_the_triple_suffix_for_the_assets_scope() {
        let artifact = OfficialArtifact {
            package: "phoxal/component-ddsm115".to_string(),
            package_name: Some("phoxal-component-ddsm115".to_string()),
            kind: ArtifactKind::Component,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::from("component/ddsm115"),
            bin_name: Some("phoxal-component-ddsm115".to_string()),
            id: "ddsm115".to_string(),
            metadata: Default::default(),
        };

        assert_eq!(
            asset_stem(&artifact, ASSETS_SCOPE),
            "phoxal-component-ddsm115-v0.1.0"
        );
    }

    /// Full package-output wiring proof: build a real participant, package the
    /// same metadata-bearing binary for two target labels, then extract and
    /// equality-check both tarballs rather than rebuilding.
    #[test]
    fn extract_metadata_from_packaged_reads_the_tarball_binary() -> Result<()> {
        let workspace = Workspace::discover()?;
        let bin_name = "phoxal-simulator-webots-controller";
        let status = Command::new("cargo")
            .args(["build", "--quiet", "-p", bin_name])
            .current_dir(workspace.root())
            .status()
            .context("failed to spawn cargo build for webots controller")?;
        assert!(status.success(), "cargo build -p {bin_name} failed");
        let binary = workspace
            .target_dir()
            .join("debug")
            .join(format!("{bin_name}{}", std::env::consts::EXE_SUFFIX));

        let artifact = OfficialArtifact {
            package: "phoxal/simulator-webots-controller".to_string(),
            package_name: Some(bin_name.to_string()),
            kind: ArtifactKind::Simulator,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::from("simulator/webots-controller"),
            bin_name: Some(bin_name.to_string()),
            id: "webots-controller".to_string(),
            metadata: Default::default(),
        };

        let dir = tempfile::tempdir().context("create tempdir")?;
        let triples = ["target-a".to_string(), "target-b".to_string()];
        for triple in &triples {
            let stem = asset_stem(&artifact, triple);
            let tarball = dir.path().join(format!("{stem}.tar.zst"));
            write_tar_zst(&tarball, &binary, bin_name)?;
        }

        let meta = extract_metadata_from_packaged(&artifact, dir.path(), &triples)?;
        assert!(meta.contracts.iter().any(|contract| {
            contract.role == "publish"
                && contract.version == "v2"
                && contract.contract == "battery::State"
                && !contract.external
        }));
        Ok(())
    }

    #[test]
    fn infrastructure_router_and_packaged_targets_have_no_participant_metadata() -> Result<()> {
        let workspace = Workspace::discover()?;
        let artifact = infrastructure_router_artifact();
        let package_name = artifact.require_package_name()?;
        let status = Command::new("cargo")
            .args(["build", "--quiet", "-p", package_name])
            .current_dir(workspace.root())
            .status()
            .context("failed to build infrastructure router")?;
        assert!(status.success(), "cargo build -p {package_name} failed");
        let binary = workspace.target_dir().join("debug").join(format!(
            "{}{}",
            artifact.require_bin_name()?,
            std::env::consts::EXE_SUFFIX
        ));
        validate_metadata_absent(&binary, &artifact)?;

        let dir = tempfile::tempdir()?;
        let targets = ["target-a".to_string(), "target-b".to_string()];
        for target in &targets {
            let tarball = dir
                .path()
                .join(format!("{}.tar.zst", asset_stem(&artifact, target)));
            write_tar_zst(&tarball, &binary, artifact.require_bin_name()?)?;
        }
        validate_no_metadata_from_packaged(&artifact, dir.path(), &targets)
    }

    #[test]
    fn infrastructure_metadata_absence_gate_rejects_a_participant_section() -> Result<()> {
        let artifact = infrastructure_router_artifact();
        let mut object = object::write::Object::new(
            object::BinaryFormat::Elf,
            object::Architecture::Aarch64,
            object::Endianness::Little,
        );
        let section = object.add_section(
            Vec::new(),
            b".phoxal_api_meta".to_vec(),
            object::SectionKind::ReadOnlyData,
        );
        object.append_section_data(
            section,
            br#"{"participant_api":"Api","contracts":[],"config_schema":{"type":"null"}}"#,
            1,
        );
        let file = tempfile::NamedTempFile::new()?;
        fs::write(file.path(), object.write()?)?;

        let error = validate_metadata_absent(file.path(), &artifact)
            .expect_err("infrastructure metadata must be rejected");
        assert!(
            error
                .to_string()
                .contains("must not embed participant API metadata"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn cross_target_metadata_mismatch_names_package_version_and_targets() -> Result<()> {
        let artifact = OfficialArtifact {
            package: "phoxal/simulator-webots-controller".to_string(),
            package_name: Some("phoxal-simulator-webots-controller".to_string()),
            kind: ArtifactKind::Simulator,
            version: "0.19.7".to_string(),
            crate_dir: PathBuf::from("simulator/webots-controller"),
            bin_name: Some("phoxal-simulator-webots-controller".to_string()),
            id: "webots-controller".to_string(),
            metadata: Default::default(),
        };
        let dir = tempfile::tempdir()?;
        let targets = [
            (
                "x86_64-unknown-linux-gnu",
                br#"{"participant_api":"Api","contracts":[],"config_schema":{"type":"null"}}"#.as_slice(),
            ),
            (
                "aarch64-unknown-linux-gnu",
                br#"{"participant_api":"Api","contracts":[{"role":"publish","version":"v1","contract":"battery::State","external":false}],"config_schema":{"type":"null"}}"#
                    .as_slice(),
            ),
        ];
        for (target, payload) in targets {
            let mut object = object::write::Object::new(
                object::BinaryFormat::Elf,
                object::Architecture::Aarch64,
                object::Endianness::Little,
            );
            let section = object.add_section(
                Vec::new(),
                b".phoxal_api_meta".to_vec(),
                object::SectionKind::ReadOnlyData,
            );
            object.append_section_data(section, payload, 1);
            let binary = dir.path().join(format!("{target}.bin"));
            fs::write(&binary, object.write()?)?;
            let tarball = dir
                .path()
                .join(format!("{}.tar.zst", asset_stem(&artifact, target)));
            write_tar_zst(&tarball, &binary, "phoxal-simulator-webots-controller")?;
        }

        let err = extract_metadata_from_packaged(
            &artifact,
            dir.path(),
            &[
                "x86_64-unknown-linux-gnu".to_string(),
                "aarch64-unknown-linux-gnu".to_string(),
            ],
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("phoxal/simulator-webots-controller v0.19.7"));
        assert!(message.contains("x86_64-unknown-linux-gnu"));
        assert!(message.contains("aarch64-unknown-linux-gnu"));
        Ok(())
    }

    #[test]
    fn extract_metadata_from_packaged_fails_when_no_tarball_present() -> Result<()> {
        let artifact = OfficialArtifact {
            package: "phoxal/simulator-webots-controller".to_string(),
            package_name: Some("phoxal-simulator-webots-controller".to_string()),
            kind: ArtifactKind::Simulator,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::from("simulator/webots-controller"),
            bin_name: Some("phoxal-simulator-webots-controller".to_string()),
            id: "webots-controller".to_string(),
            metadata: Default::default(),
        };
        let dir = tempfile::tempdir().context("create tempdir")?;
        let err = extract_metadata_from_packaged(
            &artifact,
            dir.path(),
            &["some-target-triple".to_string()],
        )
        .unwrap_err();
        assert!(err.to_string().contains("missing packaged binary"), "{err}");
        Ok(())
    }
}
