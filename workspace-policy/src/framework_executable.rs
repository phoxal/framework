//! Exact framework-owned executables outside the authored artifact catalogue.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use cargo_metadata::semver::Version;

use crate::executable::{validate_executable_targets, validate_registry_publish};

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
}

impl Spec {
    pub const fn package_name(self) -> &'static str {
        self.package_name
    }

    pub const fn manifest_path(self) -> &'static str {
        self.manifest_path
    }

    pub const fn bin_name(self) -> &'static str {
        self.bin_name
    }

    pub const fn source_path(self) -> &'static str {
        self.source_path
    }

    pub(crate) fn matches_manifest(self, relative: &Path) -> bool {
        relative == Path::new(self.manifest_path)
    }

    pub(crate) fn validate(
        self,
        package: &cargo_metadata::Package,
        root: &Path,
        manifest_path: &Path,
    ) -> Result<FrameworkExecutable> {
        let package_name = package.name.as_str();
        if package_name != self.package_name {
            bail!(
                "{} is the framework-owned root executable but package.name is \
                 '{package_name}'; expected '{}'",
                crate::executable::relative_display(root, manifest_path),
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
        )?;
        let crate_dir = manifest_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("{package_name} manifest has no parent directory"))?
            .to_path_buf();
        Ok(FrameworkExecutable {
            spec: self,
            version: package.version.clone(),
            crate_dir,
        })
    }
}

/// The exact framework-owned executables published with the framework train.
pub const SPECS: [Spec; 1] = [Spec {
    package_name: "phoxal-supervisor",
    manifest_path: "supervisor/Cargo.toml",
    bin_name: "phoxal-supervisor",
    source_path: "supervisor/src/main.rs",
}];

/// One discovered framework-owned executable.
#[derive(Clone, Debug)]
pub struct FrameworkExecutable {
    pub spec: Spec,
    pub version: Version,
    pub crate_dir: PathBuf,
}

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
