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
             to see them so their changes cut trains. Found: {}",
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
    validate_executable_targets_with_library(
        package_name,
        role,
        expected_bin,
        expected_source,
        None,
        targets,
        root,
    )
}

pub(crate) fn validate_executable_targets_with_library(
    package_name: &str,
    role: &str,
    expected_bin: &str,
    expected_source: Option<&Path>,
    expected_library: Option<(&str, &Path)>,
    targets: &[Target],
    root: &Path,
) -> Result<()> {
    if let Some((target, target_kind)) = targets.iter().find_map(|target| {
        unsupported_target_kind(target, expected_library.is_some()).map(|kind| (target, kind))
    }) {
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
    let library_targets = targets
        .iter()
        .filter(|target| target.kind.iter().any(is_library_target_kind))
        .collect::<Vec<_>>();
    match (expected_library, library_targets.as_slice()) {
        (None, []) => {}
        (Some((expected_name, expected_source)), [library]) => {
            if library.name != expected_name {
                bail!(
                    "{package_name} is {role} but its library target is '{}'; expected '{expected_name}'",
                    library.name
                );
            }
            let actual = library
                .src_path
                .as_std_path()
                .strip_prefix(root)
                .unwrap_or(library.src_path.as_std_path());
            if actual != expected_source {
                bail!(
                    "{package_name} is {role} but its library source is {}; expected {}",
                    actual.display(),
                    expected_source.display()
                );
            }
        }
        (None, [library, ..]) => bail!(
            "{package_name} is {role} but target '{}' has a library kind",
            library.name
        ),
        (Some(_), found) => bail!(
            "{package_name} is {role} but has {} library targets; expected exactly one",
            found.len()
        ),
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

fn unsupported_target_kind(target: &Target, allow_library: bool) -> Option<&TargetKind> {
    target
        .kind
        .iter()
        .find(|kind| !(is_allowed_target_kind(kind) || allow_library && **kind == TargetKind::Lib))
}

pub(crate) fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}
