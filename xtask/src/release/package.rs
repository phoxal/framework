use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::workspace::{ArtifactKind, OfficialArtifact, Workspace, require_nonempty_artifacts};

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
}

#[derive(Debug)]
pub struct PackagedArtifact {
    pub tarball: PathBuf,
    pub checksum: PathBuf,
    pub metadata: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let selected = select_artifacts(&workspace, &args)?;
    require_nonempty_artifacts(&selected)?;
    let out_dir = workspace_relative_out_dir(&workspace, &args.out);
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create output directory {}", out_dir.display()))?;
    let host_triple = host_triple(workspace.root())?;

    for artifact in selected {
        let packaged = package_artifact(&workspace, &artifact, &out_dir, &host_triple)
            .with_context(|| format!("failed to package {}", artifact.package_name))?;
        println!(
            "packaged {} v{} for {} -> {}, {}, {}",
            artifact.package_name,
            artifact.version,
            host_triple,
            packaged.tarball.display(),
            packaged.checksum.display(),
            packaged.metadata.display()
        );
    }

    Ok(())
}

fn workspace_relative_out_dir(workspace: &Workspace, out_dir: &Path) -> PathBuf {
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
    Ok(vec![workspace.official_artifact(package_name)?.clone()])
}

fn package_artifact(
    workspace: &Workspace,
    artifact: &OfficialArtifact,
    out_dir: &Path,
    host_triple: &str,
) -> Result<PackagedArtifact> {
    build_artifact(workspace.root(), artifact)?;
    let binary_path = binary_path(workspace, artifact);
    let emit_stdout = run_emit_apis(&binary_path)?;
    validate_emit_apis_json(&emit_stdout, &artifact.id, artifact.kind)?;

    let stem = asset_stem(artifact, host_triple);
    let tarball = out_dir.join(format!("{stem}.tar.zst"));
    let checksum = out_dir.join(format!("{stem}.tar.zst.sha256"));
    let metadata = out_dir.join(format!("{stem}.emit-apis.json"));

    write_tar_zst(&tarball, &binary_path, &artifact.bin_name)?;
    write_sha256(&tarball, &checksum)?;
    fs::write(&metadata, &emit_stdout)
        .with_context(|| format!("failed to write emit-apis metadata {}", metadata.display()))?;

    Ok(PackagedArtifact {
        tarball,
        checksum,
        metadata,
    })
}

fn build_artifact(root: &Path, artifact: &OfficialArtifact) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .args(["build", "-p", &artifact.package_name, "--release"])
        .current_dir(root);

    let output = command
        .output()
        .with_context(|| format!("failed to spawn cargo build for {}", artifact.package_name))?;
    if !output.status.success() {
        bail!(
            "cargo build for {} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            artifact.package_name,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

fn binary_path(workspace: &Workspace, artifact: &OfficialArtifact) -> PathBuf {
    workspace.target_dir().join("release").join(format!(
        "{}{}",
        artifact.bin_name,
        std::env::consts::EXE_SUFFIX
    ))
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

/// Returns the local host triple used in asset names. This seed packages only
/// host builds; cross-target naming and build orchestration belongs to native
/// release CI plan #01.
fn host_triple(root: &Path) -> Result<String> {
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

fn asset_stem(artifact: &OfficialArtifact, host_triple: &str) -> String {
    format!(
        "{}-v{}-{}",
        artifact.package_name, artifact.version, host_triple
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

fn sha256_file(path: &Path) -> Result<String> {
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

pub fn validate_emit_apis_json(
    stdout: &[u8],
    expected_id: &str,
    expected_kind: ArtifactKind,
) -> Result<()> {
    let metadata: ParticipantMetadata =
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
    let expected_kind = expected_kind.emit_apis_kind();
    if metadata.artifact.kind != expected_kind {
        bail!(
            "emit-apis artifact.kind '{}' did not match expected '{}'",
            metadata.artifact.kind,
            expected_kind
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
    if metadata.required_contracts.is_empty() {
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
        let topic = contract
            .topic
            .as_deref()
            .with_context(|| format!("emit-apis required_contracts[{index}] is missing topic"))?;
        if topic.trim().is_empty() {
            bail!("emit-apis required_contracts[{index}].topic must not be empty");
        }
        let direction = contract.direction.as_deref().with_context(|| {
            format!("emit-apis required_contracts[{index}] is missing direction")
        })?;
        if !DIRECTIONS.contains(&direction) {
            bail!(
                "emit-apis required_contracts[{index}].direction '{}' is not one of {}",
                direction,
                DIRECTIONS.join("|")
            );
        }
    }

    Ok(())
}

/// The `Direction` wire vocabulary the emitter serializes
/// (`phoxal/src/participant/spec.rs`, `#[serde(rename_all = "snake_case")]`).
const DIRECTIONS: [&str; 6] = [
    "publish",
    "subscribe",
    "query_request",
    "query_response",
    "server_request",
    "server_response",
];

fn is_schema_id(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Debug, Deserialize)]
struct ParticipantMetadata {
    schema: String,
    artifact: Artifact,
    api_version: String,
    participant_class: Option<String>,
    bus_abi: String,
    required_contracts: Vec<Contract>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    kind: String,
    id: String,
}

#[derive(Debug, Deserialize)]
struct Contract {
    api_version: Option<String>,
    schema_id: Option<String>,
    family: Option<String>,
    topic: Option<String>,
    direction: Option<String>,
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
      "family": "frame::LookupRequest",
      "topic": "frame/lookup",
      "direction": "query_request"
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
      "family": "frame::LookupRequest",
      "topic": "frame/lookup",
      "direction": "query_request"
    }
  ],"#,
            r#"  "required_contracts": [],"#,
        );
        let err = validate(&json).expect_err("empty required_contracts should fail");
        assert_error_contains(&err, "required_contracts");
    }

    #[test]
    fn validation_rejects_missing_contract_family() {
        let json = valid_emit().replace("      \"family\": \"frame::LookupRequest\",\n", "");
        let err = validate(&json).expect_err("missing contract family should fail");
        assert_error_contains(&err, "family");
    }

    #[test]
    fn validation_rejects_missing_contract_topic() {
        let json = valid_emit().replace("      \"topic\": \"frame/lookup\",\n", "");
        let err = validate(&json).expect_err("missing contract topic should fail");
        assert_error_contains(&err, "topic");
    }

    #[test]
    fn validation_rejects_unknown_contract_direction() {
        let json = valid_emit().replace(
            r#""direction": "query_request""#,
            r#""direction": "listen""#,
        );
        let err = validate(&json).expect_err("unknown contract direction should fail");
        assert_error_contains(&err, "direction");
    }

    #[test]
    fn asset_stem_uses_package_version_and_host_triple() {
        let artifact = OfficialArtifact {
            package_name: "phoxal-service-frame".to_string(),
            kind: ArtifactKind::Service,
            version: "0.19.1".to_string(),
            crate_dir: PathBuf::from("service/frame"),
            bin_name: "phoxal-service-frame".to_string(),
            id: "frame".to_string(),
        };

        assert_eq!(
            asset_stem(&artifact, "x86_64-unknown-linux-gnu"),
            "phoxal-service-frame-v0.19.1-x86_64-unknown-linux-gnu"
        );
    }
}
