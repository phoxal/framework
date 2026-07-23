use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use phoxal::suite::is_sha256;
use phoxal::suite::v1::{Artifact, ArtifactActivation, Blob, Kind, SCHEMA, Suite, SuiteProfiles};

use crate::release::package;
use crate::workspace::{
    ASSETS_SCOPE, ArtifactKind, LaunchProfile, OfficialArtifact, Workspace,
    require_nonempty_artifacts,
};

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
    let profiles = generate_profiles(artifacts)?;
    Ok(Suite::new(train, profiles, entries))
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
    let mut artifact_ids = BTreeSet::new();
    for artifact in &suite.artifacts {
        if !artifact_ids.insert(artifact.id.as_str()) {
            bail!("suite contains duplicate artifact {}", artifact.id);
        }
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
    validate_profiles(&suite.profiles, &suite.artifacts)?;
    Ok(())
}

fn generate_profiles(artifacts: &[OfficialArtifact]) -> Result<SuiteProfiles> {
    let mut profiles = SuiteProfiles::default();
    for artifact in artifacts {
        match &artifact.metadata.activation {
            Some(activation) => {
                if !requires_activation(artifact.kind) {
                    bail!(
                        "{} is a {} artifact and must not declare launch activation; only tool and infrastructure artifacts are profile-activated",
                        artifact.package,
                        artifact.kind
                    );
                }
                let entry = ArtifactActivation {
                    package: artifact.package.clone(),
                    scope: activation.scope,
                    criticality: activation.criticality,
                };
                for profile in &activation.profiles {
                    match profile {
                        LaunchProfile::Native => profiles.native.push(entry.clone()),
                        LaunchProfile::Webots => profiles.webots.push(entry.clone()),
                    }
                }
            }
            None if requires_activation(artifact.kind) => {
                bail!(
                    "{} is a {} artifact but has no [package.metadata.phoxal.activation] launch policy",
                    artifact.package,
                    artifact.kind
                );
            }
            None => {}
        }
    }
    profiles
        .native
        .sort_by(|left, right| left.package.cmp(&right.package));
    profiles
        .webots
        .sort_by(|left, right| left.package.cmp(&right.package));
    let artifact_kinds = artifacts
        .iter()
        .map(|artifact| (artifact.package.as_str(), kind(artifact.kind)))
        .collect::<BTreeMap<_, _>>();
    validate_profiles_with_kinds(&profiles, &artifact_kinds)?;
    Ok(profiles)
}

fn validate_profiles(profiles: &SuiteProfiles, artifacts: &[Artifact]) -> Result<()> {
    let artifact_kinds = artifacts
        .iter()
        .map(|artifact| (artifact.id.as_str(), artifact.kind))
        .collect::<BTreeMap<_, _>>();
    validate_profiles_with_kinds(profiles, &artifact_kinds)
}

fn validate_profiles_with_kinds(
    profiles: &SuiteProfiles,
    artifact_kinds: &BTreeMap<&str, Kind>,
) -> Result<()> {
    let mut activated = BTreeSet::new();
    let mut policy_by_package = BTreeMap::new();
    for (profile_name, entries) in [
        ("native", profiles.native.as_slice()),
        ("webots", profiles.webots.as_slice()),
    ] {
        let mut previous = None;
        let mut seen = BTreeSet::new();
        for entry in entries {
            if !seen.insert(entry.package.as_str()) {
                bail!(
                    "suite profile {profile_name} contains duplicate activation {}",
                    entry.package
                );
            }
            if previous.is_some_and(|package| package > entry.package.as_str()) {
                bail!("suite profile {profile_name} activations must be sorted by package");
            }
            previous = Some(entry.package.as_str());
            let artifact_kind = artifact_kinds
                .get(entry.package.as_str())
                .with_context(|| {
                    format!(
                        "suite profile {profile_name} activates missing artifact {}",
                        entry.package
                    )
                })?;
            if !matches!(artifact_kind, Kind::Tool | Kind::Infrastructure) {
                bail!(
                    "suite profile {profile_name} activates {} artifact {}; only tool and infrastructure artifacts may be activated",
                    suite_kind(*artifact_kind),
                    entry.package
                );
            }
            if let Some((scope, criticality)) =
                policy_by_package.insert(entry.package.as_str(), (entry.scope, entry.criticality))
            {
                if scope != entry.scope || criticality != entry.criticality {
                    bail!(
                        "suite profiles disagree on scope or criticality for {}",
                        entry.package
                    );
                }
            }
            activated.insert(entry.package.as_str());
        }
    }
    for (package, kind) in artifact_kinds {
        if matches!(kind, Kind::Tool | Kind::Infrastructure) && !activated.contains(package) {
            bail!(
                "suite {} artifact {} is absent from every launch profile",
                suite_kind(*kind),
                package
            );
        }
    }
    Ok(())
}

const fn requires_activation(kind: ArtifactKind) -> bool {
    matches!(kind, ArtifactKind::Tool | ArtifactKind::Infrastructure)
}

const fn suite_kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Service => "service",
        Kind::Component => "component",
        Kind::Tool => "tool",
        Kind::Simulator => "simulator",
        Kind::Infrastructure => "infrastructure",
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::suite::v1::{ActivationCriticality, ActivationScope};

    use crate::workspace::{ArtifactActivationMetadata, PhoxalPackageMetadata};

    fn official(package: &str, kind: ArtifactKind, profiles: &[LaunchProfile]) -> OfficialArtifact {
        OfficialArtifact {
            package: package.to_string(),
            package_name: Some(package.replace('/', "-")),
            kind,
            version: "0.38.0".to_string(),
            crate_dir: PathBuf::from(package),
            bin_name: Some(package.replace('/', "-")),
            id: package
                .rsplit_once('-')
                .map_or(package, |(_, id)| id)
                .to_string(),
            metadata: PhoxalPackageMetadata {
                activation: Some(ArtifactActivationMetadata {
                    profiles: profiles.to_vec(),
                    scope: if kind == ArtifactKind::Infrastructure {
                        ActivationScope::PerProject
                    } else {
                        ActivationScope::PerRobot
                    },
                    criticality: if kind == ArtifactKind::Infrastructure {
                        ActivationCriticality::Required
                    } else {
                        ActivationCriticality::Optional
                    },
                }),
                ..PhoxalPackageMetadata::default()
            },
        }
    }

    fn suite_artifact(id: &str, kind: Kind) -> Artifact {
        Artifact {
            id: id.to_string(),
            kind,
            targets: BTreeMap::from([(
                "aarch64-apple-darwin".to_string(),
                Blob {
                    url: "https://example.invalid/artifact".to_string(),
                    sha256: "a".repeat(64),
                    size: 1,
                },
            )]),
            assets: None,
        }
    }

    #[test]
    fn generation_is_sorted_and_projects_metadata_into_both_profiles() -> Result<()> {
        let artifacts = vec![
            official(
                "phoxal/tool-log",
                ArtifactKind::Tool,
                &[LaunchProfile::Webots, LaunchProfile::Native],
            ),
            official(
                "phoxal/infrastructure-router",
                ArtifactKind::Infrastructure,
                &[LaunchProfile::Native, LaunchProfile::Webots],
            ),
        ];
        let profiles = generate_profiles(&artifacts)?;
        let packages = |entries: &[ArtifactActivation]| {
            entries
                .iter()
                .map(|entry| entry.package.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            packages(&profiles.native),
            vec![
                "phoxal/infrastructure-router".to_string(),
                "phoxal/tool-log".to_string()
            ]
        );
        assert_eq!(packages(&profiles.webots), packages(&profiles.native));
        assert_eq!(
            profiles.native[0].criticality,
            ActivationCriticality::Required
        );
        assert_eq!(profiles.native[1].scope, ActivationScope::PerRobot);
        Ok(())
    }

    #[test]
    fn generation_rejects_missing_coverage_and_invalid_activation_kinds() {
        let mut missing = official(
            "phoxal/tool-log",
            ArtifactKind::Tool,
            &[LaunchProfile::Native],
        );
        missing.metadata.activation = None;
        let error = generate_profiles(&[missing]).unwrap_err();
        assert!(error.to_string().contains("has no"), "{error}");

        let service = official(
            "phoxal/service-drive",
            ArtifactKind::Service,
            &[LaunchProfile::Native],
        );
        let error = generate_profiles(&[service]).unwrap_err();
        assert!(error.to_string().contains("must not declare"), "{error}");
    }

    #[test]
    fn validation_rejects_missing_duplicate_and_uncovered_packages() {
        let missing = SuiteProfiles {
            native: vec![ArtifactActivation {
                package: "phoxal/tool-missing".to_string(),
                scope: ActivationScope::PerRobot,
                criticality: ActivationCriticality::Optional,
            }],
            webots: vec![],
        };
        let error = validate_profiles(
            &missing,
            &[suite_artifact("phoxal/service-drive", Kind::Service)],
        )
        .unwrap_err();
        assert!(error.to_string().contains("missing artifact"), "{error}");

        let duplicate = ArtifactActivation {
            package: "phoxal/tool-log".to_string(),
            scope: ActivationScope::PerRobot,
            criticality: ActivationCriticality::Optional,
        };
        let error = validate_profiles(
            &SuiteProfiles {
                native: vec![duplicate.clone(), duplicate],
                webots: vec![],
            },
            &[suite_artifact("phoxal/tool-log", Kind::Tool)],
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("duplicate activation"),
            "{error}"
        );

        let error = validate_profiles(
            &SuiteProfiles::default(),
            &[suite_artifact("phoxal/tool-log", Kind::Tool)],
        )
        .unwrap_err();
        assert!(error.to_string().contains("absent from every"), "{error}");
    }
}
