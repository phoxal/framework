//! Discovery of committed workspace source for repository-wide policy rules.
//!
//! Policy gates inspect Git's index rather than walking the filesystem. That
//! keeps editor state, ignored worktrees, build output and local scratch files
//! from changing a test result, while still including newly staged files in a
//! change under review.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Every present Git-tracked file below `root`, relative to `root`, sorted and
/// deduplicated. An unstaged deletion or broken tracked symlink has no content
/// for a source rule to inspect, so it is omitted.
pub fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", "."])
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to list tracked files below {}", root.display()))?;
    if !output.status.success() {
        bail!(
            "git ls-files failed below {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut files = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            std::str::from_utf8(path)
                .map(PathBuf::from)
                .context("a tracked path is not UTF-8")
        })
        .collect::<Result<Vec<_>>>()?;
    files.retain(|path| root.join(path).is_file());
    files.sort_unstable();
    files.dedup();
    Ok(files)
}
