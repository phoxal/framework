use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use phoxal::suite::{Artifact, Blob, Kind, SCHEMA, Suite, is_sha256};

use crate::release::package;
use crate::workspace::{ASSETS_SCOPE, ArtifactKind, Workspace, require_nonempty_artifacts};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, default_value = "target/xtask/release")]
    pub package_dir: PathBuf,
    #[arg(long, default_value = "target/xtask/suite.json")]
    pub out: PathBuf,
    /// Exact release tag, which must be `v<workspace version>`.
    #[arg(long)]
    pub tag: String,
    #[arg(long, env = "GITHUB_REPOSITORY", default_value = "phoxal/framework")]
    pub repo: String,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let suite = generate(&workspace, &args.package_dir, &args.repo, &args.tag)?;
    let out = relative(workspace.root(), &args.out);
    write(&out, &suite)?;
    verify(&suite, &args.tag)?;
    println!(
        "generated {SCHEMA} train {} with {} artifacts at {}",
        suite.version,
        suite.artifacts.len(),
        out.display()
    );
    Ok(())
}

pub(crate) fn generate(
    workspace: &Workspace,
    package_dir: &Path,
    repo: &str,
    tag: &str,
) -> Result<Suite> {
    let artifacts = workspace.official_artifacts();
    require_nonempty_artifacts(artifacts)?;
    let train = workspace_version(workspace)?;
    let expected_tag = format!("v{train}");
    if tag != expected_tag {
        bail!("release tag {tag} does not match workspace train {expected_tag}");
    }
    let package_dir = relative(workspace.root(), package_dir);
    let mut entries = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        if artifact.version != train {
            bail!(
                "{} has version {}, expected workspace train {train}",
                artifact.package,
                artifact.version
            );
        }
        let mut targets = BTreeMap::new();
        let mut assets = None;
        for target in artifact.supported_target_triples() {
            let output = package::read_packaged_output(artifact, &package_dir, &target)?;
            let blob = Blob {
                url: format!(
                    "https://github.com/{repo}/releases/download/{tag}/{}",
                    output.tarball_name
                ),
                sha256: output.tarball_sha256,
                size: output.tarball_size,
            };
            if target == ASSETS_SCOPE {
                assets = Some(blob);
            } else {
                targets.insert(target, blob);
            }
        }
        entries.push(Artifact {
            id: artifact.package.clone(),
            kind: kind(artifact.kind),
            targets,
            assets,
        });
    }
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(Suite::new(train, entries))
}

pub(crate) fn verify(suite: &Suite, tag: &str) -> Result<()> {
    if suite.schema != SCHEMA {
        bail!("suite schema is {}, expected {SCHEMA}", suite.schema);
    }
    if tag != format!("v{}", suite.version) {
        bail!(
            "suite version {} does not match release tag {tag}",
            suite.version
        );
    }
    if suite.artifacts.is_empty() {
        bail!("suite contains no official artifacts");
    }
    for artifact in &suite.artifacts {
        if artifact.targets.is_empty() {
            bail!("{} contains no target artifacts", artifact.id);
        }
        for blob in artifact.targets.values().chain(artifact.assets.iter()) {
            if !is_sha256(&blob.sha256) || blob.size == 0 {
                bail!(
                    "{} contains an invalid artifact integrity record",
                    artifact.id
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn workspace_version(workspace: &Workspace) -> Result<String> {
    let manifest = fs::read_to_string(workspace.root().join("Cargo.toml"))?;
    let value = manifest.parse::<toml_edit::DocumentMut>()?;
    value["workspace"]["package"]["version"]
        .as_str()
        .map(ToOwned::to_owned)
        .context("workspace.package.version is missing")
}

fn kind(value: ArtifactKind) -> Kind {
    match value {
        ArtifactKind::Service => Kind::Service,
        ArtifactKind::Component => Kind::Component,
        ArtifactKind::Tool => Kind::Tool,
        ArtifactKind::Simulator => Kind::Simulator,
        ArtifactKind::Infrastructure => Kind::Infrastructure,
    }
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.into()
    } else {
        root.join(path)
    }
}

fn write(path: &Path, suite: &Suite) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut json = serde_json::to_string_pretty(suite)?;
    json.push('\n');
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}
