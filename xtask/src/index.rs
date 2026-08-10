//! Which published train the workspace is compared against.
//!
//! The published crates are the baseline. Nothing is stored in the repository
//! for a comparison to read: a snapshot committed beside the code would be
//! updated by the same change it is supposed to judge, so the registry - which
//! a release cannot rewrite - is the only honest record of what shipped.

use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;

use crate::surface::CONTRACT_CRATES;

/// One version of one crate, as the registry index states it.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PublishedVersion {
    #[serde(rename = "vers")]
    version: Version,
    yanked: bool,
}

impl PublishedVersion {
    /// Read one crate's index entry: one JSON object per line, oldest first.
    fn read(document: &str) -> Result<Vec<Self>> {
        document
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .with_context(|| format!("registry index line is not a version: {line}"))
            })
            .collect()
    }
}

#[cfg(test)]
impl PublishedVersion {
    /// One version a test states as published and installable.
    pub(crate) fn published(version: &Version) -> Self {
        Self {
            version: version.clone(),
            yanked: false,
        }
    }

    /// One version a test states, with the yank state it was left in.
    fn stated(version: &str, yanked: bool) -> Self {
        Self {
            version: Version::parse(version).expect("the stated version parses"),
            yanked,
        }
    }
}

/// Where the checker reads a crate's published versions.
///
/// The sparse index is the one implementation that reaches a network; a test
/// supplies its own, so the resolution rules below are proved without one.
pub(crate) trait PublishedVersions {
    /// Every version the registry lists for `crate_name`, yanked ones
    /// included.
    fn versions(&self, crate_name: &str) -> Result<Vec<PublishedVersion>>;

    /// The latest published framework train.
    ///
    /// A train publishes as one set, so the newest version each contract crate
    /// carries is the same version for all of them. When they disagree the
    /// registry is mid-publish or a publish failed part way, and naming that
    /// state is the only honest answer: comparing against a partial train would
    /// compare today's endpoints against yesterday's documents.
    ///
    /// A yanked version was withdrawn and nobody may resolve it, so the newest
    /// version that is still installable is the newest one that is not yanked.
    fn latest_train(&self) -> Result<Version> {
        let mut latest = Vec::new();
        for contract_crate in CONTRACT_CRATES {
            let newest = self
                .versions(contract_crate.name)?
                .into_iter()
                .filter(|published| !published.yanked)
                .map(|published| published.version)
                .max()
                .with_context(|| {
                    format!(
                        "{} has no published version that is not yanked, so there is no train to \
                         compare against",
                        contract_crate.name
                    )
                })?;
            latest.push((contract_crate.name, newest));
        }

        let train = latest
            .iter()
            .map(|(_, version)| version.clone())
            .max()
            .context("no contract crate is declared, so no baseline can be resolved")?;
        if latest.iter().any(|(_, version)| *version != train) {
            let listed = latest
                .iter()
                .map(|(name, version)| format!("{name} {version}"))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "the published framework train is incomplete: the contract crates do not share a \
                 newest version ({listed}). A train publishes as one set, so comparing against a \
                 partially published train would compare one crate's contracts against another \
                 crate's previous release. Re-run once the publish finishes."
            );
        }
        Ok(train)
    }
}

/// The crates.io sparse index, read over HTTPS.
pub(crate) struct SparseIndex {
    base_url: String,
}

impl SparseIndex {
    /// The index every published contract crate lives in.
    pub(crate) fn crates_io() -> Self {
        Self {
            base_url: "https://index.crates.io".to_owned(),
        }
    }

    /// The index path a crate name maps to, as the sparse-index layout spells
    /// it.
    fn path_of(crate_name: &str) -> String {
        let name = crate_name.to_lowercase();
        match name.len() {
            1 => format!("1/{name}"),
            2 => format!("2/{name}"),
            3 => format!("3/{}/{name}", &name[..1]),
            _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
        }
    }
}

impl PublishedVersions for SparseIndex {
    fn versions(&self, crate_name: &str) -> Result<Vec<PublishedVersion>> {
        let url = format!("{}/{}", self.base_url, Self::path_of(crate_name));
        let document = ureq::get(&url)
            .call()
            .with_context(|| format!("failed to read the registry index at {url}"))?
            .body_mut()
            .read_to_string()
            .with_context(|| format!("failed to read the registry index body at {url}"))?;
        PublishedVersion::read(&document)
            .with_context(|| format!("failed to read {crate_name}'s registry index"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// A registry whose answers a test states outright.
    #[derive(Default)]
    struct FixtureIndex {
        entries: BTreeMap<&'static str, Vec<PublishedVersion>>,
    }

    impl FixtureIndex {
        /// Every contract crate publishing the same versions.
        fn train(versions: &[(&str, bool)]) -> Self {
            let mut index = Self::default();
            for contract_crate in CONTRACT_CRATES {
                index.entries.insert(
                    contract_crate.name,
                    versions
                        .iter()
                        .map(|(version, yanked)| PublishedVersion::stated(version, *yanked))
                        .collect(),
                );
            }
            index
        }

        fn with(mut self, crate_name: &'static str, versions: &[(&str, bool)]) -> Self {
            self.entries.insert(
                crate_name,
                versions
                    .iter()
                    .map(|(version, yanked)| PublishedVersion::stated(version, *yanked))
                    .collect(),
            );
            self
        }
    }

    impl PublishedVersions for FixtureIndex {
        fn versions(&self, crate_name: &str) -> Result<Vec<PublishedVersion>> {
            Ok(self.entries.get(crate_name).cloned().unwrap_or_default())
        }
    }

    /// The index states versions in publication order, which is not version
    /// order, so the baseline is the greatest and not the last line.
    #[test]
    fn the_newest_version_of_a_complete_train_is_the_baseline() {
        let index = FixtureIndex::train(&[("0.58.1", false), ("0.57.0", false), ("0.58.0", false)]);
        assert_eq!(
            index.latest_train().expect("a complete train resolves"),
            Version::new(0, 58, 1)
        );
    }

    /// A yanked release cannot be resolved by anyone, so it cannot be the
    /// contract everyone else is holding.
    #[test]
    fn a_yanked_newest_version_falls_back_to_the_previous_one() {
        let index = FixtureIndex::train(&[("0.58.0", false), ("0.58.1", true)]);
        assert_eq!(
            index.latest_train().expect("the previous train resolves"),
            Version::new(0, 58, 0)
        );
    }

    /// A half-published train is named rather than silently compared against,
    /// because the crates would then describe two different releases.
    #[test]
    fn a_partially_published_train_is_named() {
        let index = FixtureIndex::train(&[("0.58.1", false)])
            .with("phoxal-api", &[("0.58.1", false), ("0.59.0", false)]);
        let failure = index
            .latest_train()
            .expect_err("an incomplete train cannot be a baseline")
            .to_string();
        assert!(failure.contains("incomplete"), "{failure}");
        assert!(failure.contains("phoxal-api 0.59.0"), "{failure}");
        assert!(failure.contains("phoxal 0.58.1"), "{failure}");
    }

    /// A crate whose every release is yanked leaves nothing to compare
    /// against, and the message names which crate.
    #[test]
    fn a_crate_with_no_installable_version_is_named() {
        let index =
            FixtureIndex::train(&[("0.58.1", false)]).with("phoxal-bus", &[("0.58.1", true)]);
        let failure = index
            .latest_train()
            .expect_err("a fully yanked crate cannot be a baseline")
            .to_string();
        assert!(failure.contains("phoxal-bus"), "{failure}");
    }

    /// The layout is the registry's, so a crate is fetched from the path the
    /// registry actually serves it on.
    #[test]
    fn index_paths_follow_the_sparse_layout() {
        assert_eq!(SparseIndex::path_of("phoxal"), "ph/ox/phoxal");
        assert_eq!(
            SparseIndex::path_of("phoxal-runtime-contract"),
            "ph/ox/phoxal-runtime-contract"
        );
        assert_eq!(SparseIndex::path_of("abc"), "3/a/abc");
        assert_eq!(SparseIndex::path_of("ab"), "2/ab");
        assert_eq!(SparseIndex::path_of("a"), "1/a");
    }

    /// The index is newline-delimited JSON, and a version it lists is read with
    /// its yank state rather than only its number.
    #[test]
    fn an_index_entry_reads_as_versions() {
        let entry = "{\"name\":\"phoxal\",\"vers\":\"0.58.0\",\"yanked\":false}\n\
                     {\"name\":\"phoxal\",\"vers\":\"0.58.1\",\"yanked\":true}\n";
        let versions = PublishedVersion::read(entry).expect("the entry reads");
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[1].version, Version::new(0, 58, 1));
        assert!(versions[1].yanked);
    }
}
