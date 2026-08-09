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
//! grammar, [`comment_reference`] owns what a comment may name. This root owns
//! only the two facts both of them need, the workspace layout.

use std::path::Path;

use anyhow::{Context, Result};

pub mod artifact;
pub mod comment_reference;

/// The directories holding the workspace's public library crates, one crate
/// each, directly under the workspace root. A library crate's directory name
/// is its `package.name`.
///
/// These are outside the artifact grammar: they are libraries, not official
/// artifact packages, so discovery must skip them rather than reject them.
/// `the_library_crate_list_matches_the_workspace_members` fails when a library
/// crate is added to the workspace without being added here, because a missing
/// entry would silently turn that crate into a grammar violation.
pub const LIBRARY_CRATE_DIRS: [&str; 9] = [
    "phoxal",
    "phoxal-api",
    "phoxal-bus",
    "phoxal-bundle",
    "phoxal-macros",
    "phoxal-manifest",
    "phoxal-model",
    "phoxal-runtime-contract",
    "phoxal-supervisor-api",
];

/// Top-level directories whose package is never published: this policy crate
/// itself, and the fixture robot the document-loading tests stage.
///
/// Both are workspace members carrying a library target, so the completeness
/// check below would otherwise demand they appear in [`LIBRARY_CRATE_DIRS`] and
/// that their directory spell their package name. Neither applies: nothing here
/// reaches a registry, and `fixture/` is named for the authored documents it
/// holds - which those tests read by path - while its package,
/// `phoxal-fixture`, is the code that stages them.
pub(crate) const EXCLUDED_TOP_LEVEL_DIRS: [&str; 2] = ["workspace-policy", "fixture"];

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
    /// every workspace member carrying a library target must be listed here,
    /// and every listed directory must still hold one.
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
            if EXCLUDED_TOP_LEVEL_DIRS.contains(&directory) {
                continue;
            }
            assert_eq!(
                directory,
                package.name.as_str(),
                "a library crate's directory must be its package name"
            );
            discovered.push(directory.to_owned());
        }

        discovered.sort_unstable();
        let mut expected = LIBRARY_CRATE_DIRS.map(str::to_owned).to_vec();
        expected.sort_unstable();
        assert_eq!(
            discovered, expected,
            "LIBRARY_CRATE_DIRS drifted from the workspace members"
        );
        Ok(())
    }
}
