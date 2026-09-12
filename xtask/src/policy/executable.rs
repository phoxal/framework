//! Validation shared by every executable published to the `phoxal` registry.

use std::path::Path;

use anyhow::{Result, bail};
use cargo_metadata::{Target, TargetKind};

/// Registry name used by every official executable package.
pub const PHOXAL_PROVIDER: &str = "phoxal";

pub(crate) fn validate_registry_publish(
    package_name: &str,
    role: &str,
    publish: Option<&[String]>,
    root: &Path,
    manifest_path: &Path,
) -> Result<()> {
    let declared = publish.unwrap_or_default();
    if declared != [PHOXAL_PROVIDER] {
        bail!(
            "{package_name} is {role} but {} does not set publish = \
             [\"{PHOXAL_PROVIDER}\"]; executables publish to the static \
             {PHOXAL_PROVIDER} registry and never to crates.io, and release-plz must be able \
             to see them for change-driven version planning. Found: {}",
            relative_display(root, manifest_path),
            if publish.is_none() {
                "no publish field (defaults to crates.io)".to_string()
            } else {
                format!("publish = {declared:?}")
            }
        );
    }
    Ok(())
}

pub(crate) fn validate_executable_targets(
    package_name: &str,
    role: &str,
    expected_bin: &str,
    expected_source: Option<&Path>,
    targets: &[Target],
    root: &Path,
) -> Result<()> {
    if let Some((target, target_kind)) = targets
        .iter()
        .find_map(|target| unsupported_target_kind(target).map(|kind| (target, kind)))
    {
        if is_library_target_kind(target_kind) {
            bail!(
                "{package_name} is {role} but target '{}' has library kind '{target_kind}'",
                target.name
            );
        }
        bail!(
            "{package_name} is {role} but target '{}' has unsupported target kind \
             '{target_kind}'; expected bin, test, bench, example, or custom-build",
            target.name
        );
    }

    let binary_targets: Vec<_> = targets
        .iter()
        .filter(|target| target.is_kind(TargetKind::Bin))
        .collect();
    let [binary_target] = binary_targets.as_slice() else {
        bail!(
            "{package_name} is {role} but has {} binary targets; expected exactly one",
            binary_targets.len()
        );
    };
    if binary_target.name != expected_bin {
        bail!(
            "{package_name} is {role} but its only binary target is '{}'; expected \
             '{expected_bin}'",
            binary_target.name
        );
    }
    if let Some(expected_source) = expected_source {
        let actual = binary_target
            .src_path
            .as_std_path()
            .strip_prefix(root)
            .unwrap_or(binary_target.src_path.as_std_path());
        if actual != expected_source {
            bail!(
                "{package_name} is {role} but its binary source is {}; expected {}",
                actual.display(),
                expected_source.display()
            );
        }
    }
    Ok(())
}

/// The target names and authored source paths of one official service package.
pub(crate) struct ServiceTargetSpec<'a> {
    /// Package-named executable target.
    pub(crate) expected_bin: &'a str,
    /// Underscore-normalized library target.
    pub(crate) expected_lib: &'a str,
    /// Package-relative executable source path.
    pub(crate) expected_bin_source: Option<&'a Path>,
    /// Package-relative library source path.
    pub(crate) expected_lib_source: Option<&'a Path>,
}

/// Validate the two implementation targets every official service exposes.
///
/// A service package is both a reusable Runtime library and an executable
/// process.  The library and binary are one implementation, so discovery
/// requires exactly one target of each kind and pins both target names and
/// source paths to the package identity.  Test, example, bench, and build
/// targets remain allowed as development targets, just as they are for the
/// binary-only component grammar.
pub(crate) fn validate_service_targets(
    package_name: &str,
    role: &str,
    spec: ServiceTargetSpec<'_>,
    targets: &[Target],
    root: &Path,
) -> Result<()> {
    if let Some((target, target_kind)) = targets.iter().find_map(|target| {
        target
            .kind
            .iter()
            .find(|kind| !is_allowed_target_kind(kind) && **kind != TargetKind::Lib)
            .map(|kind| (target, kind))
    }) {
        if is_library_target_kind(target_kind) {
            bail!(
                "{package_name} is {role} but target '{}' has library kind '{target_kind}'; an \
                 official service must expose exactly one ordinary lib target",
                target.name
            );
        }
        bail!(
            "{package_name} is {role} but target '{}' has unsupported target kind \
             '{target_kind}'; expected lib, bin, test, bench, example, or custom-build",
            target.name
        );
    }

    let library_targets: Vec<_> = targets
        .iter()
        .filter(|target| target.is_kind(TargetKind::Lib))
        .collect();
    let [library_target] = library_targets.as_slice() else {
        bail!(
            "{package_name} is {role} but has {} library targets; expected exactly one",
            library_targets.len()
        );
    };
    if library_target.name != spec.expected_lib {
        bail!(
            "{package_name} is {role} but its only library target is '{}'; expected \
             '{}'",
            library_target.name,
            spec.expected_lib
        );
    }
    validate_target_source(
        package_name,
        role,
        "library",
        library_target,
        spec.expected_lib_source,
        root,
    )?;

    let binary_targets: Vec<_> = targets
        .iter()
        .filter(|target| target.is_kind(TargetKind::Bin))
        .collect();
    let [binary_target] = binary_targets.as_slice() else {
        bail!(
            "{package_name} is {role} but has {} binary targets; expected exactly one",
            binary_targets.len()
        );
    };
    if binary_target.name != spec.expected_bin {
        bail!(
            "{package_name} is {role} but its only binary target is '{}'; expected \
             '{}'",
            binary_target.name,
            spec.expected_bin
        );
    }
    validate_target_source(
        package_name,
        role,
        "binary",
        binary_target,
        spec.expected_bin_source,
        root,
    )?;
    Ok(())
}

fn validate_target_source(
    package_name: &str,
    role: &str,
    target_role: &str,
    target: &Target,
    expected_source: Option<&Path>,
    root: &Path,
) -> Result<()> {
    let Some(expected_source) = expected_source else {
        return Ok(());
    };
    let actual = target
        .src_path
        .as_std_path()
        .strip_prefix(root)
        .unwrap_or(target.src_path.as_std_path());
    if actual != expected_source {
        bail!(
            "{package_name} is {role} but its {target_role} source is {}; expected {}",
            actual.display(),
            expected_source.display()
        );
    }
    Ok(())
}

pub(crate) fn publishes_to_phoxal(package: &cargo_metadata::Package) -> bool {
    package
        .publish
        .as_deref()
        .is_some_and(|publish| publish == [PHOXAL_PROVIDER])
}

pub(crate) fn is_allowed_target_kind(kind: &TargetKind) -> bool {
    matches!(
        kind,
        TargetKind::Bin
            | TargetKind::Test
            | TargetKind::Bench
            | TargetKind::Example
            | TargetKind::CustomBuild
    )
}

pub(crate) fn is_library_target_kind(kind: &TargetKind) -> bool {
    matches!(
        kind,
        TargetKind::Lib
            | TargetKind::RLib
            | TargetKind::DyLib
            | TargetKind::CDyLib
            | TargetKind::StaticLib
            | TargetKind::ProcMacro
    )
}

fn unsupported_target_kind(target: &Target) -> Option<&TargetKind> {
    target
        .kind
        .iter()
        .find(|kind| !is_allowed_target_kind(kind))
}

pub(crate) fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}
