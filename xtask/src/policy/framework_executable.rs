//! Exact framework-owned executables outside the authored artifact catalogue.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::artifact::ArtifactKind;
use super::executable::{PHOXAL_PROVIDER, validate_executable_targets, validate_registry_publish};
use super::{Subject, Violation};

/// One permitted framework-owned executable package.
///
/// This is an explicit tuple rather than an extensible kind grammar: the
/// These are framework infrastructure, never services, components, catalogue
/// entries, or bundle participants.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Spec {
    package_name: &'static str,
    manifest_path: &'static str,
    bin_name: &'static str,
    source_path: &'static str,
    forbidden_dependencies: &'static [&'static str],
}

impl Spec {
    pub const fn package_name(self) -> &'static str {
        self.package_name
    }

    pub const fn manifest_path(self) -> &'static str {
        self.manifest_path
    }

    /// Dependencies that would move authoring or parsing policy into this
    /// framework-owned executable.
    pub const fn forbidden_dependencies(self) -> &'static [&'static str] {
        self.forbidden_dependencies
    }

    pub(crate) fn matches_manifest(self, relative: &Path) -> bool {
        relative == Path::new(self.manifest_path)
    }

    /// Whether `package` is the executable this spec describes, exactly.
    pub(crate) fn validate(
        self,
        package: &cargo_metadata::Package,
        root: &Path,
        manifest_path: &Path,
    ) -> Result<()> {
        let package_name = package.name.as_str();
        if package_name != self.package_name {
            bail!(
                "{} is the framework-owned root executable but package.name is \
                 '{package_name}'; expected '{}'",
                super::executable::relative_display(root, manifest_path),
                self.package_name
            );
        }
        validate_registry_publish(
            package_name,
            "the framework-owned root executable",
            package.publish.as_deref(),
            root,
            manifest_path,
        )?;
        validate_executable_targets(
            package_name,
            "the framework-owned root executable",
            self.bin_name,
            Some(Path::new(self.source_path)),
            &package.targets,
            root,
        )
    }
}

/// The exact framework-owned executables published with the framework release
/// set.
pub const SPECS: [Spec; 4] = [
    Spec {
        package_name: "phoxal-supervisor",
        manifest_path: "supervisor/Cargo.toml",
        bin_name: "phoxal-supervisor",
        source_path: "supervisor/src/main.rs",
        forbidden_dependencies: &[
            // The supervisor is built from the one framework library, never from
            // its former CLI owner.
            "phoxal-cli",
            // Authored YAML/URDF and their parsers stop at bundle compilation. The
            // `authoring` feature that would pull them in is refused separately, by
            // the dependency rule that covers every official participant.
            "serde_yaml",
            "urdf-rs",
        ],
    },
    Spec {
        package_name: "phoxal-simulator-webots-host",
        manifest_path: "simulators/webots/host/Cargo.toml",
        bin_name: "phoxal-simulator-webots-host",
        source_path: "simulators/webots/host/src/main.rs",
        forbidden_dependencies: &["phoxal-cli", "serde_yaml", "urdf-rs"],
    },
    Spec {
        package_name: "phoxal-simulator-webots-world-controller",
        manifest_path: "simulators/webots/world-controller/Cargo.toml",
        bin_name: "phoxal-simulator-webots-world-controller",
        source_path: "simulators/webots/world-controller/src/main.rs",
        forbidden_dependencies: &["phoxal-cli", "serde_yaml", "urdf-rs"],
    },
    Spec {
        package_name: "phoxal-simulator-webots-robot-controller",
        manifest_path: "simulators/webots/robot-controller/Cargo.toml",
        bin_name: "phoxal-simulator-webots-robot-controller",
        source_path: "simulators/webots/robot-controller/src/main.rs",
        forbidden_dependencies: &["phoxal-cli", "serde_yaml", "urdf-rs"],
    },
];

pub(crate) fn spec_for_manifest(root: &Path, manifest_path: &Path) -> Option<Spec> {
    let relative = manifest_path.strip_prefix(root).ok()?;
    SPECS
        .iter()
        .copied()
        .find(|spec| spec.matches_manifest(relative))
}

pub(crate) fn spec_for_package(package_name: &str) -> Option<Spec> {
    SPECS
        .iter()
        .copied()
        .find(|spec| spec.package_name == package_name)
}

/// The framework-owned executable set's place in the workspace. The supervisor is an ordinary
/// default member that plain root cargo commands build, publishing to the
/// `phoxal` registry, carrying none of the authoring or parser dependencies its
/// spec forbids, and standing outside the artifact catalogue rather than inside
/// it under a kind of its own.
pub(super) fn the_supervisor_is_a_default_member_and_non_catalog_executable(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    let Some(supervisor) = subject
        .members
        .workspace_packages()
        .into_iter()
        .find(|package| package.name.as_str() == SPECS[0].package_name())
    else {
        return Ok(vec![Violation::new(format!(
            "{} is absent from the workspace members",
            SPECS[0].package_name()
        ))]);
    };

    if !subject.members.workspace_members.contains(&supervisor.id) {
        violations.push(Violation::new(format!(
            "{} is not a workspace member",
            supervisor.name
        )));
    }
    if !subject
        .members
        .workspace_default_members
        .contains(&supervisor.id)
    {
        violations.push(Violation::new(format!(
            "plain root cargo commands must build {}, so it must be a default member",
            supervisor.name
        )));
    }
    if supervisor
        .publish
        .as_deref()
        .is_none_or(|publish| publish != [PHOXAL_PROVIDER])
    {
        violations.push(Violation::new(format!(
            "{} does not publish to the {PHOXAL_PROVIDER} registry alone",
            supervisor.name
        )));
    }
    let manifest = supervisor.manifest_path.as_std_path();
    if manifest.strip_prefix(&subject.root) != Ok(Path::new(SPECS[0].manifest_path())) {
        violations.push(Violation::new(format!(
            "{} declares its manifest at {}; expected {}",
            supervisor.name,
            manifest.display(),
            SPECS[0].manifest_path()
        )));
    }
    for dependency in &supervisor.dependencies {
        if SPECS[0]
            .forbidden_dependencies()
            .contains(&dependency.name.as_str())
        {
            violations.push(Violation::new(format!(
                "{} has the forbidden authoring or parser dependency {}",
                supervisor.name, dependency.name
            )));
        }
    }

    let workspace = match subject.executables() {
        Ok(workspace) => workspace,
        Err(violation) => {
            violations.push(violation);
            return Ok(violations);
        }
    };
    let found = workspace
        .framework_executables()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let expected = SPECS.into_iter().collect::<BTreeSet<_>>();
    if found != expected {
        violations.push(Violation::new(format!(
            "the workspace framework-owned executable set is not exact; found {:?}",
            found
                .iter()
                .map(|spec| spec.package_name())
                .collect::<Vec<_>>()
        )));
    }
    for artifact in workspace.official_artifacts() {
        if SPECS
            .iter()
            .any(|spec| artifact.package_name() == spec.package_name())
        {
            violations.push(Violation::new(format!(
                "{} entered the artifact catalogue; it is framework infrastructure and never a \
                 catalogue entry",
                artifact.package_name()
            )));
        }
    }
    // The exact executable is not a kind: an artifact directory named after it
    // would put it back in the catalogue through the grammar's front door.
    if ArtifactKind::try_from("supervisor").is_ok() {
        violations.push(Violation::new(
            "'supervisor' names an artifact kind; the framework-owned executable must stay \
             outside the artifact grammar",
        ));
    }
    Ok(violations)
}

/// Every package published to the `phoxal` registry has an independent Cargo
/// version and is covered by the workspace release policy.
///
/// The policy is global rather than a second package catalogue. Release-plz
/// discovers changed packages from Cargo metadata, leaves unrelated packages
/// untouched, and prepares one reviewable PR with each selected package's own
/// version. Package tables may override unrelated release-plz defaults, but
/// cannot reintroduce a version group or a package that is skipped by release
/// processing.
pub(super) fn registry_packages_have_independent_release_policy(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let registry_packages = subject
        .members
        .workspace_packages()
        .into_iter()
        .filter(|package| {
            package
                .publish
                .as_deref()
                .is_some_and(|publish| publish == [PHOXAL_PROVIDER])
        })
        .map(|package| package.name.to_string())
        .collect::<BTreeSet<_>>();

    let mut violations = Vec::new();
    if !registry_packages.contains(SPECS[0].package_name()) {
        violations.push(Violation::new(format!(
            "{} does not publish to the {PHOXAL_PROVIDER} registry",
            SPECS[0].package_name()
        )));
    }

    for package in subject.members.workspace_packages() {
        if !registry_packages.contains(package.name.as_str()) {
            continue;
        }
        let manifest = package.manifest_path.clone().into_std_path_buf();
        let source = fs::read_to_string(&manifest)
            .with_context(|| format!("failed to read {}'s manifest", package.name))?;
        if source
            .lines()
            .any(|line| line.trim_start().starts_with("version.workspace"))
        {
            violations.push(Violation::new(format!(
                "{} inherits its version from the workspace; published packages must own independent version fields",
                package.name
            )));
        }
    }

    let release_source = fs::read_to_string(subject.root.join("release-plz.toml"))
        .context("failed to read release-plz.toml")?;
    let release = release_source
        .parse::<toml_edit::DocumentMut>()
        .context("release-plz.toml is invalid")?;

    let workspace = release["workspace"]
        .as_table()
        .context("release-plz.toml has no [workspace] table")?;
    for (field, expected) in [
        ("release_always", false),
        ("changelog_update", false),
        ("publish", false),
        ("git_tag_enable", false),
        ("git_release_enable", false),
    ] {
        if workspace.get(field).and_then(toml_edit::Item::as_bool) != Some(expected) {
            violations.push(Violation::new(format!(
                "release-plz [workspace] must set {field} = {expected}"
            )));
        }
    }
    if workspace
        .get("dependencies_update")
        .and_then(toml_edit::Item::as_bool)
        != Some(false)
    {
        violations.push(Violation::new(
            "release-plz [workspace] must set dependencies_update = false",
        ));
    }

    if let Some(packages) = release
        .get("package")
        .and_then(toml_edit::Item::as_array_of_tables)
    {
        for package in packages {
            let Some(name) = package.get("name").and_then(toml_edit::Item::as_str) else {
                continue;
            };
            if !registry_packages.contains(name) {
                continue;
            }
            for field in ["version_group", "release"] {
                if package.get(field).is_some() {
                    violations.push(Violation::new(format!(
                        "{name} must not override release-plz {field}; package selection is independent and change-driven"
                    )));
                }
            }
            for field in [
                "publish",
                "changelog_update",
                "git_tag_enable",
                "git_release_enable",
            ] {
                if package.get(field).and_then(toml_edit::Item::as_bool) == Some(true) {
                    violations.push(Violation::new(format!(
                        "{name} must not enable release-plz {field}"
                    )));
                }
            }
        }
    }

    let workflow = fs::read_to_string(subject.root.join(".github/workflows/release-plz.yml"))
        .context("failed to read the release workflow")?;
    if !workflow.contains("rust-release-plz-pr.yml@main") {
        violations.push(Violation::new(
            "release-plz workflow must use the shared release-plz PR workflow",
        ));
    }
    for stale in [
        "framework-train",
        "version_group",
        "TRAIN",
        "cargo xtask compatibility",
        "release-plz release",
    ] {
        if workflow.contains(stale) {
            violations.push(Violation::new(format!(
                "release-plz workflow still contains the retired synchronized-release marker {stale:?}"
            )));
        }
    }
    if workflow.contains("crates.io") || workflow.contains("crates_io") {
        violations.push(Violation::new(
            "framework release workflow must not publish rewritten packages to crates.io",
        ));
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;

    #[test]
    fn the_checked_in_release_surface_is_independent_and_review_only() -> Result<()> {
        let subject = Subject::for_workspace()?;
        let violations = registry_packages_have_independent_release_policy(&subject)?;
        assert!(
            violations.is_empty(),
            "unexpected release policy findings: {violations:?}"
        );
        Ok(())
    }
}
