//! Exact framework-owned executables outside the authored artifact catalogue.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::artifact::ArtifactKind;
use super::executable::{PHOXAL_PROVIDER, validate_executable_targets, validate_registry_publish};
use super::{Subject, Violation};

/// One permitted framework-owned root executable.
///
/// This is an explicit tuple rather than an extensible kind grammar: the
/// supervisor is framework infrastructure, never a service, component,
/// simulator, catalogue entry, or runtime-graph participant.
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

/// The exact framework-owned executables published with the framework train.
pub const SPECS: [Spec; 1] = [Spec {
    package_name: "phoxal-supervisor",
    manifest_path: "supervisor/Cargo.toml",
    bin_name: "phoxal-supervisor",
    source_path: "supervisor/src/main.rs",
    forbidden_dependencies: &[
        // The supervisor consumes finalized runtime contracts, never the
        // participant facade or its former CLI owner.
        "phoxal",
        "phoxal-cli",
        // Authored YAML/URDF and their parsers stop at bundle compilation.
        "phoxal-manifest",
        "serde_yaml",
        "urdf-rs",
    ],
}];

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

/// The framework-owned supervisor's place in the workspace: an ordinary
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
    match workspace.framework_executables() {
        [only] if *only == SPECS[0] => {}
        found => violations.push(Violation::new(format!(
            "the workspace must declare exactly the one framework-owned executable; found {:?}",
            found
                .iter()
                .map(|spec| spec.package_name())
                .collect::<Vec<_>>()
        ))),
    }
    for artifact in workspace.official_artifacts() {
        if artifact.package_name() == SPECS[0].package_name() {
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

/// Every package published to the `phoxal` registry moves with the framework
/// train, and moves through the neutral executable batch rather than a
/// hand-named one: a package release-plz cannot see cannot cut a train, and a
/// workflow that names a package by hand stops covering the next one.
pub(super) fn every_registry_executable_participates_in_the_release_train(
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

    let release_source = fs::read_to_string(subject.root.join("release-plz.toml"))
        .context("failed to read release-plz.toml")?;
    let release = release_source
        .parse::<toml_edit::DocumentMut>()
        .context("release-plz.toml is invalid")?;
    let packages = release["package"]
        .as_array_of_tables()
        .context("release-plz.toml has no package list")?;
    let mut configured = BTreeSet::new();
    for package in packages {
        let Some(name) = package.get("name").and_then(toml_edit::Item::as_str) else {
            continue;
        };
        if !registry_packages.contains(name) {
            continue;
        }
        if package
            .get("version_group")
            .and_then(toml_edit::Item::as_str)
            != Some("framework-train")
        {
            violations.push(Violation::new(format!(
                "{name} must move with the framework train"
            )));
        }
        for flag in [
            "publish",
            "changelog_update",
            "git_tag_enable",
            "git_release_enable",
        ] {
            if package.get(flag).and_then(toml_edit::Item::as_bool) != Some(false) {
                violations.push(Violation::new(format!("{name} must set {flag} = false")));
            }
        }
        if package.get("release").is_some() {
            violations.push(Violation::new(format!(
                "{name} must participate in release-plz change detection"
            )));
        }
        configured.insert(name.to_owned());
    }
    for name in registry_packages.difference(&configured) {
        violations.push(Violation::new(format!(
            "{name} publishes to the {PHOXAL_PROVIDER} registry but release-plz.toml does not \
             configure it"
        )));
    }

    let workflow = fs::read_to_string(subject.root.join(".github/workflows/release-plz.yml"))
        .context("failed to read the release workflow")?;
    if workflow.matches("select(.publish == [\"phoxal\"])").count() < 4 {
        violations.push(Violation::new(
            "registry packaging and verification must remain metadata-driven in \
             .github/workflows/release-plz.yml",
        ));
    }
    if workflow.contains("cargo package -p phoxal-supervisor") {
        violations.push(Violation::new(
            "the supervisor must use the neutral executable batch in \
             .github/workflows/release-plz.yml",
        ));
    }
    Ok(violations)
}
