//! Workspace policy: the rules the framework workspace must obey as a whole,
//! and the tests that enforce them.
//!
//! No single crate owns these facts - that a package's directory, name and
//! `publish` field agree, that the zenoh dependency set keeps transport
//! compression disabled, that no committed comment carries an issue or
//! decision reference - so they live here, in a crate whose only purpose is to
//! be a test target under `cargo test --workspace`.
//!
//! Each rule lives in the module that owns it: [`artifact`] owns the package
//! grammar, [`comment_reference`] owns what a comment may name, and
//! [`tracked_source`] owns what counts as committed source for repository-wide
//! scans. This root owns only the workspace facts they share.

use std::path::Path;

use anyhow::{Context, Result};

pub mod artifact;
pub mod comment_reference;
pub mod tracked_source;

/// The directory holding every library crate that carries a name suffix.
pub const LIBRARY_CRATE_ROOT: &str = "crates";

/// The facade crate, which is both a directory at the workspace root and the
/// package name every other library crate prefixes itself with.
pub const FACADE: &str = "phoxal";

/// The directories holding the workspace's published library crates, one
/// crate each, as paths relative to the workspace root.
///
/// These are outside the artifact grammar: they are libraries, not official
/// artifact packages, so discovery must skip them rather than reject them.
/// `the_library_crate_list_matches_the_workspace_members` fails when a library
/// crate is added to the workspace without being added here, because a missing
/// entry would silently turn that crate into a grammar violation.
pub const LIBRARY_CRATE_DIRS: [&str; 9] = [
    "phoxal",
    "crates/api",
    "crates/bundle",
    "crates/bus",
    "crates/macros",
    "crates/manifest",
    "crates/model",
    "crates/runtime-contract",
    "crates/supervisor-api",
];

/// The package a library crate directory must hold, or `None` for a directory
/// that names no library crate location.
///
/// One rule, no exceptions: a library crate is `phoxal-<suffix>` and lives at
/// `crates/<suffix>`. The facade is the single crate whose name carries no
/// suffix, so it is the single crate that does not live in the suffix
/// directory; it sits at the workspace root as `phoxal/`. That falls out of
/// the rule rather than carving a hole in it.
///
/// This is the whole reason the directory can be shortened at all. `crates/`
/// already says `phoxal`, so repeating it in every child would be the
/// provider spelled twice on one path.
pub fn library_package_name(directory: &str) -> Option<String> {
    if directory == FACADE {
        return Some(FACADE.to_owned());
    }
    let suffix = directory
        .strip_prefix(LIBRARY_CRATE_ROOT)?
        .strip_prefix('/')?;
    // A library crate is one directory deep and no deeper: `crates/api/inner`
    // would be a second package hiding under the first one's name.
    if suffix.is_empty() || suffix.contains('/') {
        return None;
    }
    Some(format!("{FACADE}-{suffix}"))
}

/// Whether a package name is one of the workspace's published library crates.
///
/// Derived from [`LIBRARY_CRATE_DIRS`] through the same rule rather than
/// listed a second time, so the directories stay the single place a library
/// crate is declared. Deliberately excludes [`INTERNAL_CRATE_DIRS`]: an
/// unpublished crate has no place in the published dependency graph.
pub fn is_library_package(package_name: &str) -> bool {
    LIBRARY_CRATE_DIRS
        .iter()
        .any(|directory| library_package_name(directory).as_deref() == Some(package_name))
}

/// Library crates that serve this workspace's own tests and reach no
/// registry. They carry a library target, so the completeness check below
/// still demands they be listed; they are simply listed here rather than in
/// [`LIBRARY_CRATE_DIRS`], which keeps them out of the published dependency
/// graph [`is_library_package`] describes.
///
/// `crates/fixture` obeys the library naming rule exactly - `phoxal-fixture`
/// at `crates/fixture` - and appears here only because it is unpublished, not
/// because it sits anywhere unusual.
pub const INTERNAL_CRATE_DIRS: [&str; 2] = [POLICY_CRATE_DIR, "crates/fixture"];

/// This policy crate: the one library crate outside the naming rule, and the
/// only directory [`library_package_name`] deliberately fails to resolve.
///
/// It is the machinery that judges the workspace's crates rather than one of
/// the crates being judged, so it carries no provider prefix and sits at the
/// root instead of under [`LIBRARY_CRATE_ROOT`]. It also has to: it resolves
/// [`workspace_root`] as its own parent, which only holds at the top level.
pub const POLICY_CRATE_DIR: &str = "workspace-policy";

/// The workspace root these rules apply to: the parent of this crate's own
/// manifest directory.
///
/// Resolved from the compile-time manifest path rather than from `cargo
/// metadata`, so a test can name the workspace before it has parsed anything
/// in it.
pub fn workspace_root() -> Result<&'static Path> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("workspace-policy manifest directory has no workspace parent")
}

#[cfg(test)]
mod tests {
    use cargo_metadata::MetadataCommand;

    use super::*;
    use crate::artifact::is_library_target_kind;

    /// A hand-maintained list that silently skips validation when it goes
    /// stale is worse than no list, so the workspace itself is the authority:
    /// every workspace member carrying a library target must be listed as
    /// either published or internal, and every listed directory must still
    /// hold one. Both lists obey the naming rule, so being unpublished buys a
    /// crate no leniency about where it lives - only this policy crate is
    /// outside the rule, and it says so by name.
    #[test]
    fn the_library_crate_list_matches_the_workspace_members() -> Result<()> {
        let root = workspace_root()?;
        let metadata = MetadataCommand::new()
            .manifest_path(root.join("Cargo.toml"))
            .no_deps()
            .exec()
            .context("failed to read workspace metadata")?;

        let mut discovered = Vec::new();
        for package in metadata.workspace_packages() {
            if !package
                .targets
                .iter()
                .any(|target| target.kind.iter().any(is_library_target_kind))
            {
                continue;
            }
            let crate_dir = package
                .manifest_path
                .parent()
                .with_context(|| format!("{} manifest has no parent", package.name))?
                .as_std_path();
            let relative = crate_dir.strip_prefix(root).with_context(|| {
                format!("{} is not under the workspace root", crate_dir.display())
            })?;
            let directory = relative
                .to_str()
                .with_context(|| format!("{} is not a UTF-8 workspace path", relative.display()))?;
            if directory != POLICY_CRATE_DIR {
                assert_eq!(
                    library_package_name(directory).as_deref(),
                    Some(package.name.as_str()),
                    "library crate {directory} does not hold the package its \
                     directory names; a library crate is `phoxal-<suffix>` at \
                     `crates/<suffix>`, or the facade `phoxal` at the root"
                );
            }
            discovered.push(directory.to_owned());
        }

        discovered.sort_unstable();
        let mut expected = LIBRARY_CRATE_DIRS
            .iter()
            .chain(INTERNAL_CRATE_DIRS.iter())
            .map(|directory| (*directory).to_owned())
            .collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(
            discovered, expected,
            "the library crate lists drifted from the workspace members"
        );
        Ok(())
    }

    /// The rule is stated once as a function, so the cases that must *not*
    /// resolve are as much a part of it as the cases that must. A directory
    /// deeper than one level is the interesting one: it would otherwise let a
    /// second package hide under the first one's name.
    #[test]
    fn a_library_crate_directory_names_exactly_one_package() {
        assert_eq!(library_package_name("phoxal").as_deref(), Some("phoxal"));
        assert_eq!(
            library_package_name("crates/api").as_deref(),
            Some("phoxal-api")
        );
        // Being unpublished changes nothing about where a crate lives.
        assert_eq!(
            library_package_name("crates/fixture").as_deref(),
            Some("phoxal-fixture")
        );
        assert_eq!(
            library_package_name("crates/runtime-contract").as_deref(),
            Some("phoxal-runtime-contract")
        );

        assert_eq!(library_package_name("crates"), None);
        assert_eq!(library_package_name("crates/"), None);
        assert_eq!(library_package_name("crates/api/inner"), None);
        assert_eq!(library_package_name("phoxal-api"), None);
        assert_eq!(library_package_name("services/drive"), None);
        assert_eq!(library_package_name("cratesfoo"), None);
    }

    /// Every listed directory must satisfy the rule, so neither list can
    /// become a place to smuggle a crate past it. `workspace-policy` is the
    /// one directory that may not - it is named as the exception rather than
    /// slipping through as one.
    #[test]
    fn every_listed_library_crate_directory_obeys_the_rule() {
        for directory in LIBRARY_CRATE_DIRS
            .iter()
            .chain(INTERNAL_CRATE_DIRS.iter())
            .filter(|directory| **directory != POLICY_CRATE_DIR)
        {
            assert!(
                library_package_name(directory).is_some(),
                "{directory} is listed as a library crate but names no package"
            );
        }
        assert_eq!(library_package_name(POLICY_CRATE_DIR), None);
    }
}
