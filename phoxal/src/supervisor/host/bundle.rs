//! Reading the supervisor's sole persisted input, and locating the run
//! directory that owns it.
//!
//! Opening a bundle parses `manifest.json` and stops there. There is nothing
//! else to check: integrity lives in the archive, where `phoxal build` writes
//! `build.phoxal` beside its `.sha256` and `phoxal install` refuses a mismatch.
//! Once a bundle is on disk the supervisor trusts what is there, exactly as
//! every participant reading the same file does. A second fence here would only
//! re-check bytes nobody re-signed.
//!
//! There is no compatibility check either. The supervisor no longer launches
//! anything, so it is not the executor of the bundle's artifacts; a client that
//! wants to know which train built this supervisor asks `supervisor/connect`,
//! which is the endpoint that exists to answer exactly that.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crate::bundle::RuntimeBundle;

/// The bundle directory's name inside a deployment release. The supervisor is
/// handed a bundle root and knows nothing about releases, but it does have to
/// find the run directory that owns the execution, and a release's bundle
/// always sits one level inside the release.
const RELEASE_BUNDLE_DIR: &str = "bundle";

/// Where a source project keeps its own release, relative to the project root.
const PROJECT_RELEASE_SUFFIX: [&str; 2] = [".phoxal", "release"];

/// Read the bundle at `root`.
pub(crate) fn open(root: &Path) -> Result<RuntimeBundle> {
    RuntimeBundle::open(root).with_context(|| {
        format!(
            "phoxal-supervisor takes a compiled bundle directory; {} is not one",
            root.display()
        )
    })
}

/// The root whose volatile run directory owns this bundle.
///
/// A bundle inside a deployment release is owned by whatever owns the release:
/// a project keeps its release at `<project>/.phoxal/release`, and every other
/// release - an installed one under `/var/phoxal`, or an extracted archive - is
/// its own root. A bare bundle root, run outside any release, owns itself.
pub(crate) fn owning_root(bundle_root: &Path) -> PathBuf {
    let Some(release_root) = strip_tail(bundle_root, &[RELEASE_BUNDLE_DIR]) else {
        return bundle_root.to_path_buf();
    };
    strip_tail(&release_root, &PROJECT_RELEASE_SUFFIX).unwrap_or(release_root)
}

/// `path` without `tail`, or `None` when it does not end with it.
fn strip_tail(path: &Path, tail: &[&str]) -> Option<PathBuf> {
    let mut components = path.components().rev();
    let found: Vec<_> = components
        .by_ref()
        .take(tail.len())
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let expected: Vec<_> = tail.iter().rev().map(ToString::to_string).collect();
    (found == expected).then(|| components.rev().collect::<PathBuf>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_without_a_manifest_is_not_a_bundle() {
        let dir = tempfile::tempdir().expect("temporary directory");
        std::fs::write(dir.path().join("robot.yaml"), "schema: phoxal/robot/v0\n")
            .expect("source fixture");
        let error = open(dir.path()).expect_err("authored YAML is not a compiled bundle");
        assert!(format!("{error:#}").contains("manifest.json"), "{error:#}");
    }

    /// The run directory a bundle's execution belongs to, for each shape a
    /// bundle root arrives in. The installed cases matter most: the unit starts
    /// `/var/phoxal/phoxal-supervisor /var/phoxal/bundle`, and the execution's
    /// socket and locks belong to the release, never to a directory inside the
    /// immutable release itself.
    #[test]
    fn a_bundle_is_owned_by_whatever_owns_the_release_it_sits_in() {
        assert_eq!(
            owning_root(Path::new("/work/rover/.phoxal/release/bundle")),
            Path::new("/work/rover")
        );
        assert_eq!(
            owning_root(Path::new("/var/phoxal/bundle")),
            Path::new("/var/phoxal")
        );
        assert_eq!(
            owning_root(Path::new("/var/lib/phoxal/releases/current/bundle")),
            Path::new("/var/lib/phoxal/releases/current")
        );
        // A bundle root that is not inside a release owns itself.
        assert_eq!(
            owning_root(Path::new("/var/lib/phoxal/releases/current")),
            Path::new("/var/lib/phoxal/releases/current")
        );
    }
}
