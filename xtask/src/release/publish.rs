use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::release::github::{self, GitHubRelease};
use crate::release::plan::{self, ReleasePlan};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(
        long,
        value_name = "PATH",
        default_value = "target/xtask/release-plan.json"
    )]
    pub plan: PathBuf,
    #[arg(long, value_name = "OWNER/REPO", env = "GITHUB_REPOSITORY")]
    pub repo: String,
    /// Print the releases that would be published without editing GitHub.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let plan = plan::load_release_plan(&args.plan)?;
    let publisher = GhPublisher;
    let published = publish_plan_with(&args.repo, &plan, args.dry_run, &publisher)?;
    let action = if args.dry_run {
        "would publish"
    } else {
        "published"
    };
    println!(
        "artifact release publish: {} draft release(s) {action}",
        published.len(),
    );
    Ok(())
}

trait Publisher {
    fn resolve_release(&self, repo: &str, tag: &str) -> Result<GitHubRelease>;
    fn publish_release(&self, repo: &str, tag: &str) -> Result<()>;
}

struct GhPublisher;

impl Publisher for GhPublisher {
    fn resolve_release(&self, repo: &str, tag: &str) -> Result<GitHubRelease> {
        github::resolve_release(repo, tag)
    }

    fn publish_release(&self, repo: &str, tag: &str) -> Result<()> {
        let status = Command::new("gh")
            .args([
                "release",
                "edit",
                tag,
                "--repo",
                repo,
                "--draft=false",
                "--latest=false",
            ])
            .status()
            .with_context(|| format!("failed to spawn gh release edit for {repo} {tag}"))?;
        if !status.success() {
            bail!("gh release edit failed for {repo} {tag} with status {status}");
        }
        Ok(())
    }
}

fn publish_plan_with<P: Publisher>(
    repo: &str,
    plan: &ReleasePlan,
    dry_run: bool,
    publisher: &P,
) -> Result<BTreeSet<String>> {
    let mut published = BTreeSet::new();
    for artifact in &plan.artifacts {
        let release = publisher.resolve_release(repo, &artifact.tag)?;
        if !release.draft {
            println!(
                "artifact release {} ({}) is already published",
                artifact.tag, artifact.package
            );
            continue;
        }
        if dry_run {
            println!(
                "would publish artifact draft release {} ({})",
                artifact.tag, artifact.package
            );
        } else {
            publisher.publish_release(repo, &artifact.tag)?;
        }
        published.insert(artifact.tag.clone());
    }
    Ok(published)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use crate::release::plan::{ReleaseMatrix, ReleasePlanArtifact};
    use crate::workspace::ArtifactKind;

    use super::*;

    #[derive(Default)]
    struct FakePublisher {
        releases: BTreeMap<String, GitHubRelease>,
        published: RefCell<Vec<String>>,
    }

    impl Publisher for FakePublisher {
        fn resolve_release(&self, _repo: &str, tag: &str) -> Result<GitHubRelease> {
            self.releases
                .get(tag)
                .cloned()
                .with_context(|| format!("missing fake release {tag}"))
        }

        fn publish_release(&self, _repo: &str, tag: &str) -> Result<()> {
            self.published.borrow_mut().push(tag.to_string());
            Ok(())
        }
    }

    fn release(tag: &str, draft: bool) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.to_string(),
            draft,
            assets: Vec::new(),
        }
    }

    fn artifact(package: &str, tag: &str) -> ReleasePlanArtifact {
        ReleasePlanArtifact {
            package: package.to_string(),
            version: "0.1.0".to_string(),
            tag: tag.to_string(),
            kind: ArtifactKind::Service,
            target_triples: vec!["x86_64-unknown-linux-gnu".to_string()],
        }
    }

    fn plan() -> ReleasePlan {
        ReleasePlan {
            schema: plan::RELEASE_PLAN_SCHEMA.to_string(),
            artifacts: vec![
                artifact("phoxal-service-drive", "phoxal-service-drive-v0.19.3"),
                artifact("phoxal-service-map", "phoxal-service-map-v0.19.3"),
            ],
            matrix: ReleaseMatrix {
                include: Vec::new(),
            },
        }
    }

    #[test]
    fn publish_plan_only_edits_draft_releases() -> Result<()> {
        let mut fake = FakePublisher::default();
        fake.releases.insert(
            "phoxal-service-drive-v0.19.3".to_string(),
            release("phoxal-service-drive-v0.19.3", true),
        );
        fake.releases.insert(
            "phoxal-service-map-v0.19.3".to_string(),
            release("phoxal-service-map-v0.19.3", false),
        );

        let published = publish_plan_with("phoxal/framework", &plan(), false, &fake)?;

        assert_eq!(
            fake.published.into_inner(),
            vec!["phoxal-service-drive-v0.19.3".to_string()]
        );
        assert_eq!(
            published,
            ["phoxal-service-drive-v0.19.3".to_string()].into()
        );
        Ok(())
    }

    #[test]
    fn dry_run_does_not_edit_draft_releases() -> Result<()> {
        let mut fake = FakePublisher::default();
        fake.releases.insert(
            "phoxal-service-drive-v0.19.3".to_string(),
            release("phoxal-service-drive-v0.19.3", true),
        );
        fake.releases.insert(
            "phoxal-service-map-v0.19.3".to_string(),
            release("phoxal-service-map-v0.19.3", true),
        );

        let published = publish_plan_with("phoxal/framework", &plan(), true, &fake)?;

        assert!(fake.published.into_inner().is_empty());
        assert_eq!(published.len(), 2);
        Ok(())
    }
}
