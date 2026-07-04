use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::catalog::verify::verify_catalog_path;
use crate::workspace::Workspace;

pub(crate) const DEFAULT_CATALOG_REF: &str = "artifact-catalog-v0-stable";

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(
        long,
        value_name = "PATH",
        default_value = "target/xtask/catalog/phoxal-artifact-catalog.json"
    )]
    pub catalog: PathBuf,
    #[arg(
        long,
        value_name = "OWNER/REPO",
        env = "GITHUB_REPOSITORY",
        default_value = "phoxal/framework"
    )]
    pub repo: String,
    #[arg(long = "ref", value_name = "BRANCH", default_value = DEFAULT_CATALOG_REF)]
    pub git_ref: String,
    /// Verify the catalog and print the publish URL without touching git.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let catalog_path = if args.catalog.is_absolute() {
        args.catalog.clone()
    } else {
        workspace.root().join(&args.catalog)
    };
    let revision = verify_catalog_path(&catalog_path)?;
    let latest_url = stable_catalog_url(&args.repo, &args.git_ref);
    let revision_filename = revision_filename(&revision.integrity.sha256);

    if args.dry_run {
        println!(
            "would publish catalog {} to {} and revisions/{}",
            revision.revision, latest_url, revision_filename
        );
        return Ok(());
    }

    let publish_dir = workspace
        .root()
        .join("target/xtask/catalog-publish")
        .join(args.git_ref.replace('/', "_"));
    prepare_worktree(workspace.root(), &publish_dir, &args.git_ref)?;
    write_catalog_files(&publish_dir, &catalog_path, &revision_filename)?;
    commit_and_push(&publish_dir, &args.git_ref, &revision.revision)?;
    println!("published catalog {} to {}", revision.revision, latest_url);
    Ok(())
}

pub(crate) fn stable_catalog_url(repo: &str, git_ref: &str) -> String {
    format!("https://raw.githubusercontent.com/{repo}/{git_ref}/latest.json")
}

fn revision_filename(sha256: &str) -> String {
    format!("sha256-{sha256}.json")
}

fn prepare_worktree(root: &Path, publish_dir: &Path, git_ref: &str) -> Result<()> {
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(publish_dir)
        .current_dir(root)
        .output();
    if publish_dir.exists() {
        fs::remove_dir_all(publish_dir)
            .with_context(|| format!("failed to remove {}", publish_dir.display()))?;
    }

    let remote_ref = format!("refs/remotes/origin/{git_ref}");
    let fetch_refspec = format!("+refs/heads/{git_ref}:{remote_ref}");
    let fetch = Command::new("git")
        .args(["fetch", "origin", &fetch_refspec])
        .current_dir(root)
        .output()
        .context("failed to spawn git fetch for catalog ref")?;

    if fetch.status.success() {
        run_git(
            root,
            &[
                "worktree",
                "add",
                "--force",
                "-B",
                git_ref,
                path_str(publish_dir)?,
                &remote_ref,
            ],
            "git worktree add catalog ref",
        )?;
    } else {
        run_git(
            root,
            &[
                "worktree",
                "add",
                "--force",
                "--detach",
                path_str(publish_dir)?,
                "HEAD",
            ],
            "git worktree add detached catalog ref",
        )?;
        run_git(
            publish_dir,
            &["checkout", "--orphan", git_ref],
            "git checkout orphan catalog ref",
        )?;
        clear_worktree_files(publish_dir)?;
    }
    Ok(())
}

fn write_catalog_files(
    publish_dir: &Path,
    catalog_path: &Path,
    revision_filename: &str,
) -> Result<()> {
    let latest = publish_dir.join("latest.json");
    let revision_dir = publish_dir.join("revisions");
    let revision_path = revision_dir.join(revision_filename);
    fs::create_dir_all(&revision_dir)
        .with_context(|| format!("failed to create {}", revision_dir.display()))?;
    let catalog_bytes = fs::read(catalog_path)
        .with_context(|| format!("failed to read {}", catalog_path.display()))?;

    if revision_path.exists() {
        let existing = fs::read(&revision_path)
            .with_context(|| format!("failed to read {}", revision_path.display()))?;
        if existing != catalog_bytes {
            bail!(
                "immutable catalog revision {} already exists with different contents",
                revision_path.display()
            );
        }
    } else {
        fs::write(&revision_path, &catalog_bytes)
            .with_context(|| format!("failed to write {}", revision_path.display()))?;
    }
    fs::write(&latest, catalog_bytes)
        .with_context(|| format!("failed to write {}", latest.display()))?;
    Ok(())
}

fn commit_and_push(publish_dir: &Path, git_ref: &str, revision: &str) -> Result<()> {
    run_git(
        publish_dir,
        &["config", "user.name", "phoxal-release-bot"],
        "git config user.name",
    )?;
    run_git(
        publish_dir,
        &["config", "user.email", "release-bot@phoxal.com"],
        "git config user.email",
    )?;
    run_git(
        publish_dir,
        &["add", "latest.json", "revisions"],
        "git add catalog",
    )?;
    let status = run_git(publish_dir, &["status", "--porcelain"], "git status")?;
    if status.stdout.is_empty() {
        println!("catalog ref is already up to date");
        return Ok(());
    }
    let message = format!("Publish artifact catalog {revision}");
    run_git(
        publish_dir,
        &["commit", "-m", &message],
        "git commit catalog",
    )?;
    run_git(
        publish_dir,
        &["push", "origin", &format!("HEAD:refs/heads/{git_ref}")],
        "git push catalog ref",
    )?;
    Ok(())
}

fn clear_worktree_files(publish_dir: &Path) -> Result<()> {
    for entry in fs::read_dir(publish_dir)
        .with_context(|| format!("failed to read {}", publish_dir.display()))?
    {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        } else {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

fn run_git(cwd: &Path, args: &[&str], label: &str) -> Result<Output> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to spawn {label}"))?;
    if !output.status.success() {
        bail!(
            "{label} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output)
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("{} is not valid UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_url_is_github_raw_branch_location() {
        assert_eq!(
            stable_catalog_url("phoxal/framework", DEFAULT_CATALOG_REF),
            "https://raw.githubusercontent.com/phoxal/framework/artifact-catalog-v0-stable/latest.json"
        );
    }
}
