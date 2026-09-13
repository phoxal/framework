//! Workspace-wide discovery for publication policy.

use std::collections::BTreeSet;

use anyhow::{Result, bail};
// Reaching a workspace by manifest path is the fixture tests' entry alone: a
// rule reads the one metadata the run already has.
#[cfg(test)]
use anyhow::Context;
#[cfg(test)]
use cargo_metadata::MetadataCommand;

use super::executable::{
    PHOXAL_PROVIDER, publishes_to_phoxal, relative_display, validate_executable_targets,
    validate_registry_publish,
};
use super::framework_executable::{SPECS, Spec, spec_for_manifest, spec_for_package};
use super::{
    artifact::{OfficialArtifact, discover_package},
    is_internal_package_directory, is_library_directory, is_library_package, library_package_name,
};

const CARGO_PHOXAL_PACKAGE: &str = "cargo-phoxal";
const CARGO_PHOXAL_MANIFEST: &str = "tools/cargo-phoxal/Cargo.toml";
const CARGO_PHOXAL_BIN: &str = "cargo-phoxal";
const CARGO_PHOXAL_SOURCE: &str = "tools/cargo-phoxal/src/main.rs";

/// The disjoint executable sets declared by the workspace.
///
/// Authored artifacts are catalogue/graph participants. Framework executables
/// and the developer tool publish through the same registry, but never enter
/// that catalogue.
#[derive(Debug)]
pub struct Workspace {
    official_artifacts: Vec<OfficialArtifact>,
    framework_executables: Vec<Spec>,
}

impl Workspace {
    /// Read and validate all executable packages of the workspace `command`
    /// names, including completeness of the exact framework-owned executable
    /// list.
    ///
    /// Only the fixture tests below reach a workspace by manifest path; a rule
    /// reads the one metadata the run already has through
    /// [`Self::from_metadata`].
    #[cfg(test)]
    pub fn discover(command: &mut MetadataCommand) -> Result<Self> {
        let metadata = command
            .no_deps()
            .exec()
            .context("failed to read cargo metadata")?;
        Self::from_metadata(&metadata)
    }

    /// The same validation over metadata a caller has already read, so a run
    /// that several rules stand on asks Cargo once.
    pub fn from_metadata(metadata: &cargo_metadata::Metadata) -> Result<Self> {
        let root = metadata.workspace_root.clone().into_std_path_buf();
        let mut official_artifacts = Vec::new();
        let mut framework_executables = Vec::new();

        for package in metadata.workspace_packages() {
            let manifest_path = package.manifest_path.clone().into_std_path_buf();
            let relative_manifest = manifest_path.strip_prefix(&root).map_err(|_| {
                anyhow::anyhow!("{} is outside the workspace root", manifest_path.display())
            })?;
            let relative_directory = relative_manifest
                .parent()
                .and_then(|directory| directory.to_str())
                .unwrap_or_default();
            if let Some(spec) = spec_for_manifest(&root, &manifest_path) {
                spec.validate(package, &root, &manifest_path)?;
                framework_executables.push(spec);
                continue;
            }
            if let Some(spec) = spec_for_package(package.name.as_str()) {
                bail!(
                    "{} declares the framework-owned executable package '{}'; expected its one \
                     manifest at {}",
                    relative_display(&root, &manifest_path),
                    package.name,
                    spec.manifest_path()
                );
            }
            if relative_manifest == std::path::Path::new(CARGO_PHOXAL_MANIFEST) {
                validate_cargo_phoxal(package, &root, &manifest_path)?;
                continue;
            }
            if package.name.as_str() == CARGO_PHOXAL_PACKAGE {
                bail!(
                    "{} declares the developer tool package '{}'; expected its one manifest at {}",
                    relative_display(&root, &manifest_path),
                    package.name,
                    CARGO_PHOXAL_MANIFEST
                );
            }
            if let Some(artifact) = discover_package(&root, package)? {
                official_artifacts.push(artifact);
                continue;
            }
            if is_internal_package_directory(relative_directory) {
                if package.publish.as_deref() != Some(&[]) {
                    bail!(
                        "{} is an internal fixture but does not set publish = false",
                        relative_display(&root, &manifest_path)
                    );
                }
                continue;
            }
            if is_library_directory(relative_directory) {
                let expected_name = library_package_name(relative_directory);
                if expected_name.as_deref() == Some(package.name.as_str())
                    && is_library_package(package.name.as_str())
                {
                    continue;
                }
                bail!(
                    "{} declares a library package at {}, but that directory maps to {:?}",
                    package.name,
                    relative_directory,
                    expected_name
                );
            }
            if is_library_package(package.name.as_str()) {
                bail!(
                    "{} declares a published library package from {}, but its package directory is not classified",
                    package.name,
                    relative_display(&root, &manifest_path)
                );
            }
            if publishes_to_phoxal(package) {
                bail!(
                    "{} publishes to the {PHOXAL_PROVIDER} registry from {}, but it is neither an \
                     authored artifact nor an explicitly listed framework-owned executable",
                    package.name,
                    relative_display(&root, &manifest_path)
                );
            }
        }

        official_artifacts.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.id.cmp(&right.id))
        });
        framework_executables.sort_by_key(|spec| spec.package_name());

        let found = framework_executables
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let missing = SPECS
            .iter()
            .filter(|spec| !found.contains(spec))
            .map(|spec| format!("{} at {}", spec.package_name(), spec.manifest_path()))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            bail!(
                "framework-owned executable(s) are absent from Cargo workspace metadata: {}",
                missing.join(", ")
            );
        }

        Ok(Self {
            official_artifacts,
            framework_executables,
        })
    }

    pub fn official_artifacts(&self) -> &[OfficialArtifact] {
        &self.official_artifacts
    }

    pub fn framework_executables(&self) -> &[Spec] {
        &self.framework_executables
    }
}

fn validate_cargo_phoxal(
    package: &cargo_metadata::Package,
    root: &std::path::Path,
    manifest_path: &std::path::Path,
) -> Result<()> {
    if package.name.as_str() != CARGO_PHOXAL_PACKAGE {
        bail!(
            "{} is the developer tool manifest but package.name is '{}'; expected '{}'",
            relative_display(root, manifest_path),
            package.name,
            CARGO_PHOXAL_PACKAGE
        );
    }
    validate_registry_publish(
        CARGO_PHOXAL_PACKAGE,
        "the Phoxal developer tool",
        package.publish.as_deref(),
        root,
        manifest_path,
    )?;
    validate_executable_targets(
        CARGO_PHOXAL_PACKAGE,
        "the Phoxal developer tool",
        CARGO_PHOXAL_BIN,
        Some(std::path::Path::new(CARGO_PHOXAL_SOURCE)),
        &package.targets,
        root,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn discover_single_package(
        directory: &str,
        package_name: &str,
        publish: &str,
        targets: &str,
    ) -> Result<Workspace> {
        let workspace_dir = tempfile::tempdir().context("create executable policy workspace")?;
        let root = workspace_dir.path();
        fs::write(
            root.join("Cargo.toml"),
            format!("[workspace]\nresolver = \"3\"\nmembers = [\"{directory}\"]\n"),
        )?;
        let package_dir = root.join(directory);
        fs::create_dir_all(package_dir.join("src"))?;
        fs::write(package_dir.join("src/main.rs"), "fn main() {}\n")?;
        fs::write(package_dir.join("src/other.rs"), "fn main() {}\n")?;
        fs::write(
            package_dir.join("src/lib.rs"),
            "//! invalid library target\n",
        )?;
        fs::write(
            package_dir.join("Cargo.toml"),
            format!(
                r#"[package]
name = "{package_name}"
version = "0.1.0"
edition = "2024"
license = "AGPL-3.0-only"
publish = {publish}
autobins = false
autolib = false

{targets}"#
            ),
        )?;
        Workspace::discover(MetadataCommand::new().manifest_path(root.join("Cargo.toml")))
    }

    #[test]
    fn exact_framework_executables_are_discovered_outside_the_artifact_catalogue() -> Result<()> {
        let workspace_dir = tempfile::tempdir().context("create executable policy workspace")?;
        let root = workspace_dir.path();
        let members = SPECS
            .iter()
            .map(|spec| {
                Path::new(spec.manifest_path())
                    .parent()
                    .expect("spec manifest has a parent")
                    .display()
                    .to_string()
            })
            .chain(std::iter::once(
                Path::new(CARGO_PHOXAL_MANIFEST)
                    .parent()
                    .expect("developer tool manifest has a parent")
                    .display()
                    .to_string(),
            ))
            .collect::<Vec<_>>();
        fs::write(
            root.join("Cargo.toml"),
            format!(
                "[workspace]\nresolver = \"3\"\nmembers = {}\n",
                serde_json::to_string(&members)?
            ),
        )?;
        for spec in SPECS {
            let manifest = root.join(spec.manifest_path());
            let directory = manifest.parent().context("spec manifest has no parent")?;
            fs::create_dir_all(directory.join("src"))?;
            fs::write(directory.join("src/main.rs"), "fn main() {}\n")?;
            fs::write(
                manifest,
                format!(
                    "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"AGPL-3.0-only\"\npublish = [\"phoxal\"]\nautobins = false\nautolib = false\n\n[[bin]]\nname = \"{}\"\npath = \"src/main.rs\"\n",
                    spec.package_name(),
                    spec.package_name()
                ),
            )?;
        }
        let tool_manifest = root.join(CARGO_PHOXAL_MANIFEST);
        fs::create_dir_all(
            tool_manifest
                .parent()
                .expect("tool manifest has a parent")
                .join("src"),
        )?;
        fs::write(
            &tool_manifest,
            format!(
                "[package]\nname = \"{CARGO_PHOXAL_PACKAGE}\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"AGPL-3.0-only\"\npublish = [\"phoxal\"]\nautobins = false\nautolib = false\n\n[[bin]]\nname = \"{CARGO_PHOXAL_BIN}\"\npath = \"src/main.rs\"\n"
            ),
        )?;
        fs::write(root.join(CARGO_PHOXAL_SOURCE), "fn main() {}\n")?;
        let workspace =
            Workspace::discover(MetadataCommand::new().manifest_path(root.join("Cargo.toml")))?;
        assert!(workspace.official_artifacts().is_empty());
        assert_eq!(
            workspace
                .framework_executables()
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            SPECS.into_iter().collect::<BTreeSet<_>>()
        );
        Ok(())
    }

    #[test]
    fn missing_framework_executable_is_a_discovery_failure() -> Result<()> {
        let workspace_dir = tempfile::tempdir().context("create empty policy workspace")?;
        fs::write(
            workspace_dir.path().join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = []\n",
        )?;
        let error = Workspace::discover(
            MetadataCommand::new().manifest_path(workspace_dir.path().join("Cargo.toml")),
        )
        .expect_err("the exact framework executable list is complete");
        assert!(
            error.to_string().contains(
                "framework-owned executable(s) are absent from Cargo workspace metadata: \
                 phoxal-supervisor at supervisor/Cargo.toml"
            ),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn framework_executable_lookalikes_cannot_enter_the_registry() {
        let valid_target = "[[bin]]\nname = \"phoxal-supervisor\"\npath = \"src/main.rs\"\n";
        let cases = [
            (
                "wrong-package",
                "supervisor",
                "phoxal-supervisor-copy",
                "[\"phoxal\"]",
                valid_target,
                "package.name is 'phoxal-supervisor-copy'; expected 'phoxal-supervisor'",
            ),
            (
                "wrong-bin",
                "supervisor",
                "phoxal-supervisor",
                "[\"phoxal\"]",
                "[[bin]]\nname = \"supervisor\"\npath = \"src/main.rs\"\n",
                "only binary target is 'supervisor'; expected 'phoxal-supervisor'",
            ),
            (
                "wrong-source",
                "supervisor",
                "phoxal-supervisor",
                "[\"phoxal\"]",
                "[[bin]]\nname = \"phoxal-supervisor\"\npath = \"src/other.rs\"\n",
                "binary source is supervisor/src/other.rs; expected supervisor/src/main.rs",
            ),
            (
                "library-target",
                "supervisor",
                "phoxal-supervisor",
                "[\"phoxal\"]",
                "[lib]\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"phoxal-supervisor\"\npath = \"src/main.rs\"\n",
                "target 'phoxal_supervisor' has library kind 'lib'",
            ),
            (
                "wrong-registry",
                "supervisor",
                "phoxal-supervisor",
                "false",
                valid_target,
                "does not set publish = [\"phoxal\"]",
            ),
            (
                "wrong-path",
                "tools/supervisor",
                "phoxal-supervisor",
                "[\"phoxal\"]",
                valid_target,
                "expected its one manifest at supervisor/Cargo.toml",
            ),
            (
                "unlisted-root-executable",
                "executor",
                "phoxal-executor",
                "[\"phoxal\"]",
                "[[bin]]\nname = \"phoxal-executor\"\npath = \"src/main.rs\"\n",
                "neither an authored artifact nor an explicitly listed framework-owned executable",
            ),
            (
                "published-internal-fixture",
                "crates/contract-consumer-fixture",
                "phoxal-contract-consumer-fixture",
                "[\"phoxal\"]",
                "[[bin]]\nname = \"phoxal-contract-consumer-fixture\"\npath = \"src/main.rs\"\n",
                "internal fixture but does not set publish = false",
            ),
        ];

        for (case, directory, package, publish, targets, expected) in cases {
            let error = discover_single_package(directory, package, publish, targets)
                .expect_err("the executable lookalike must be rejected");
            assert!(
                error.to_string().contains(expected),
                "{case} produced an unexpected error: {error:#}"
            );
        }
    }
}
