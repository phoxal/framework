use std::collections::BTreeSet;
use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GitHubRelease {
    pub tag_name: String,
    pub draft: bool,
    pub assets: Vec<GitHubReleaseAsset>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GitHubReleaseAsset {
    pub name: String,
}

impl GitHubRelease {
    pub(crate) fn asset_names(&self) -> BTreeSet<String> {
        self.assets.iter().map(|asset| asset.name.clone()).collect()
    }
}

pub(crate) fn resolve_release(repo: &str, tag: &str) -> Result<GitHubRelease> {
    if let Some(release) = release_by_tag(repo, tag)? {
        return Ok(release);
    }

    release_from_list(repo, tag)?
        .with_context(|| format!("GitHub release for tag {tag} does not exist in {repo}"))
}

pub(crate) fn release_asset_names(repo: &str, tag: &str) -> Result<BTreeSet<String>> {
    Ok(resolve_release(repo, tag)?.asset_names())
}

fn release_by_tag(repo: &str, tag: &str) -> Result<Option<GitHubRelease>> {
    let path = format!("repos/{repo}/releases/tags/{tag}");
    let output = gh_api(&["--method", "GET", &path])
        .with_context(|| format!("failed to spawn gh api release-by-tag for {repo} {tag}"))?;
    if output.status.success() {
        return release_from_json(&output.stdout)
            .with_context(|| {
                format!("gh api release-by-tag returned invalid JSON for {repo} {tag}")
            })
            .map(Some);
    }
    if gh_api_not_found(&output) {
        return Ok(None);
    }
    Err(gh_failure("gh api release-by-tag", repo, tag, &output))
}

fn release_from_list(repo: &str, tag: &str) -> Result<Option<GitHubRelease>> {
    let path = format!("repos/{repo}/releases?per_page=100");
    let output = gh_api(&["--method", "GET", "--paginate", "--slurp", &path])
        .with_context(|| format!("failed to spawn gh api release list for {repo} {tag}"))?;
    if !output.status.success() {
        return Err(gh_failure("gh api release list", repo, tag, &output));
    }
    release_from_list_json(&output.stdout, tag)
        .with_context(|| format!("gh api release list returned invalid JSON for {repo}"))
}

fn gh_api(args: &[&str]) -> Result<Output> {
    Command::new("gh")
        .arg("api")
        .args(args)
        .output()
        .context("failed to spawn gh api")
}

fn gh_api_not_found(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    stderr.contains("HTTP 404") || stdout.contains("HTTP 404")
}

fn gh_failure(action: &str, repo: &str, tag: &str, output: &Output) -> anyhow::Error {
    anyhow!(
        "{action} failed for {repo} {tag}\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn release_from_json(bytes: &[u8]) -> Result<GitHubRelease> {
    serde_json::from_slice(bytes).context("release JSON was not valid")
}

fn release_from_list_json(bytes: &[u8], tag: &str) -> Result<Option<GitHubRelease>> {
    let releases = releases_from_list_json(bytes)?;
    Ok(releases.into_iter().find(|release| release.tag_name == tag))
}

fn releases_from_list_json(bytes: &[u8]) -> Result<Vec<GitHubRelease>> {
    if let Ok(pages) = serde_json::from_slice::<Vec<Vec<GitHubRelease>>>(bytes) {
        return Ok(pages.into_iter().flatten().collect());
    }
    serde_json::from_slice(bytes).context("release list JSON was not valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_asset_names_parse_by_tag_json() -> Result<()> {
        let release = release_from_json(
            br#"{
  "tag_name": "phoxal-service-drive-v0.19.3",
  "draft": false,
  "assets": [
    { "name": "drive-linux.tar.zst" },
    { "name": "drive-linux.tar.zst.sha256" }
  ]
}"#,
        )?;

        assert_eq!(
            release.asset_names(),
            ["drive-linux.tar.zst", "drive-linux.tar.zst.sha256"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
        Ok(())
    }

    #[test]
    fn release_list_json_finds_draft_by_tag() -> Result<()> {
        let release = release_from_list_json(
            br#"[
  [
    {
      "tag_name": "phoxal-service-drive-v0.19.3",
      "draft": true,
      "assets": [
        { "name": "drive-linux.tar.zst" }
      ]
    }
  ]
]"#,
            "phoxal-service-drive-v0.19.3",
        )?
        .context("expected release")?;

        assert!(release.draft);
        assert_eq!(
            release.asset_names(),
            ["drive-linux.tar.zst".to_string()].into()
        );
        Ok(())
    }

    #[test]
    fn release_list_json_reports_missing_tag() -> Result<()> {
        let release = release_from_list_json(
            br#"[
  {
    "tag_name": "phoxal-service-drive-v0.19.3",
    "draft": true,
    "assets": []
  }
]"#,
            "phoxal-service-map-v0.19.3",
        )?;

        assert!(release.is_none());
        Ok(())
    }
}
