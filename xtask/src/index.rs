//! Which published train the workspace is compared against.
//!
//! The published crates are the baseline. Nothing is stored in the repository
//! for a comparison to read: a snapshot committed beside the code would be
//! updated by the same change it is supposed to judge, so the registry - which
//! a release cannot rewrite - is the only honest record of what shipped.

use anyhow::{Context, Result};
use semver::Version;
use serde::Deserialize;

use crate::surface::CONTRACT_CRATE;
use crate::toolchain::RustVersion;

/// One version of one crate, as the registry index states it.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PublishedVersion {
    #[serde(rename = "vers")]
    version: Version,
    yanked: bool,
    /// The toolchain floor this release was published with.
    ///
    /// Optional because the index carries the field only for crates published
    /// after it was added, and a crate that states no floor promises nothing
    /// about a toolchain rather than promising the lowest one.
    #[serde(default)]
    rust_version: Option<String>,
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
            rust_version: Some("1.88".to_owned()),
        }
    }

    /// One version a test states, with the yank state it was left in.
    fn stated(version: &str, yanked: bool) -> Self {
        Self::with_floor(version, yanked, Some("1.88"))
    }

    /// One version a test states, with the toolchain floor it went out under.
    fn with_floor(version: &str, yanked: bool, rust_version: Option<&str>) -> Self {
        Self {
            version: Version::parse(version).expect("the stated version parses"),
            yanked,
            rust_version: rust_version.map(str::to_owned),
        }
    }
}

/// The published train a comparison is made against.
///
/// The version and the toolchain floor come from the same index entry, so a
/// baseline is one fact rather than two that could describe different releases.
#[derive(Clone, Debug)]
pub(crate) struct PublishedTrain {
    /// The version the one contract crate carries.
    pub(crate) version: Version,
    /// The toolchain floor it was published under, when it states one.
    pub(crate) rust_version: Option<RustVersion>,
}

impl PublishedTrain {
    /// One train stated outright, by a drill that reads no registry.
    pub(crate) const fn stated(version: Version, rust_version: Option<RustVersion>) -> Self {
        Self {
            version,
            rust_version,
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
    /// The framework is one library, so a train is one crate's release and the
    /// newest one is the baseline outright. There is nothing to reconcile: the
    /// half-published state a multi-crate train could be caught in - today's
    /// endpoints against yesterday's documents - is not a state one package can
    /// be in.
    ///
    /// A yanked version was withdrawn and nobody may resolve it, so the newest
    /// version that is still installable is the newest one that is not yanked.
    ///
    /// The train's toolchain floor is read from the same entry, so the floor
    /// belongs to the release the comparison actually pins.
    fn latest_train(&self) -> Result<PublishedTrain> {
        let newest = self
            .versions(CONTRACT_CRATE)?
            .into_iter()
            .filter(|published| !published.yanked)
            .max_by(|left, right| left.version.cmp(&right.version))
            .with_context(|| {
                format!(
                    "{CONTRACT_CRATE} has no published version that is not yanked, so there is no \
                     train to compare against"
                )
            })?;
        let rust_version = newest
            .rust_version
            .as_deref()
            .map(RustVersion::parse)
            .transpose()
            .with_context(|| {
                format!(
                    "{CONTRACT_CRATE} {} states an unreadable rust-version in the registry",
                    newest.version
                )
            })?;
        Ok(PublishedTrain {
            version: newest.version,
            rust_version,
        })
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
        let mut response = match ureq::get(&url).call() {
            Ok(response) => response,
            // A sparse-index 404 is the registry's representation of a package
            // with zero history, which is an answer rather than a read failure.
            // It reaches the caller as an empty history, so a crate that has
            // never been published is reported as having no baseline instead of
            // as an unreachable index.
            Err(ureq::Error::StatusCode(404)) => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read the registry index at {url}"));
            }
        };
        let document = response
            .body_mut()
            .read_to_string()
            .with_context(|| format!("failed to read the registry index body at {url}"))?;
        PublishedVersion::read(&document)
            .with_context(|| format!("failed to read {crate_name}'s registry index"))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::*;

    /// A registry whose answers a test states outright.
    #[derive(Default)]
    struct FixtureIndex {
        entries: BTreeMap<&'static str, Vec<PublishedVersion>>,
        queries: RefCell<Vec<String>>,
    }

    impl FixtureIndex {
        /// The one contract crate publishing these versions.
        fn published(versions: &[(&str, bool)]) -> Self {
            Self::default().with(CONTRACT_CRATE, versions)
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

        /// Versions published under stated toolchain floors.
        fn with_floor(
            mut self,
            crate_name: &'static str,
            versions: &[(&str, Option<&str>)],
        ) -> Self {
            self.entries.insert(
                crate_name,
                versions
                    .iter()
                    .map(|(version, floor)| PublishedVersion::with_floor(version, false, *floor))
                    .collect(),
            );
            self
        }
    }

    impl PublishedVersions for FixtureIndex {
        fn versions(&self, crate_name: &str) -> Result<Vec<PublishedVersion>> {
            self.queries.borrow_mut().push(crate_name.to_owned());
            Ok(self.entries.get(crate_name).cloned().unwrap_or_default())
        }
    }

    /// The index states versions in publication order, which is not version
    /// order, so the baseline is the greatest and not the last line.
    #[test]
    fn the_newest_published_version_is_the_baseline() {
        let index =
            FixtureIndex::published(&[("0.65.0", false), ("0.64.0", false), ("0.63.0", false)]);
        let train = index.latest_train().expect("a train resolves");
        assert_eq!(train.version, Version::new(0, 65, 0));
        assert_eq!(
            train.rust_version,
            Some(RustVersion::parse("1.88").expect("the floor parses"))
        );
    }

    /// One library means one registry entry: nothing else is read, so no other
    /// package's history can move the baseline or its floor.
    #[test]
    fn only_the_one_contract_crate_is_read() {
        let index = FixtureIndex::published(&[("0.65.0", false)])
            .with("phoxal-protocol", &[("0.99.0", false)])
            .with("phoxal-macros", &[("0.99.0", false)]);
        let train = index.latest_train().expect("a train resolves");
        assert_eq!(train.version, Version::new(0, 65, 0));
        assert_eq!(*index.queries.borrow(), [CONTRACT_CRATE]);
    }

    /// The floor comes from the resolved release's own entry, so a newer
    /// release's floor is the baseline's floor and an older one's is not.
    #[test]
    fn the_baseline_floor_is_the_one_the_resolved_release_published_under() {
        let index = FixtureIndex::default().with_floor(
            CONTRACT_CRATE,
            &[("0.64.0", Some("1.85")), ("0.65.0", Some("1.88"))],
        );
        let train = index.latest_train().expect("a train resolves");
        assert_eq!(
            train.rust_version,
            Some(RustVersion::parse("1.88").expect("the floor parses"))
        );
    }

    /// A train older than the index field states no floor, which is an answer
    /// rather than a failure: it promises nothing about a toolchain.
    #[test]
    fn a_train_that_states_no_floor_resolves_without_one() {
        let index = FixtureIndex::default().with_floor(CONTRACT_CRATE, &[("0.50.0", None)]);
        let train = index.latest_train().expect("a train resolves");
        assert_eq!(train.version, Version::new(0, 50, 0));
        assert_eq!(train.rust_version, None);
    }

    /// A floor the registry states in a spelling no toolchain has is named
    /// rather than silently dropped: the axis would otherwise stop gating.
    #[test]
    fn an_unreadable_floor_is_named_with_its_release() {
        let index =
            FixtureIndex::default().with_floor(CONTRACT_CRATE, &[("0.65.0", Some("nightly"))]);
        let failure = index
            .latest_train()
            .expect_err("an unreadable floor cannot be compared")
            .to_string();
        assert!(failure.contains("phoxal 0.65.0"), "{failure}");
    }

    /// A yanked release cannot be resolved by anyone, so it cannot be the
    /// contract everyone else is holding.
    #[test]
    fn a_yanked_newest_version_falls_back_to_the_previous_one() {
        let index = FixtureIndex::published(&[("0.64.0", false), ("0.65.0", true)]);
        assert_eq!(
            index
                .latest_train()
                .expect("the previous release resolves")
                .version,
            Version::new(0, 64, 0)
        );
    }

    /// A crate whose every release is yanked leaves nothing to compare
    /// against, and the message names it.
    #[test]
    fn a_crate_with_no_installable_version_is_named() {
        let index = FixtureIndex::published(&[("0.65.0", true)]);
        let failure = index
            .latest_train()
            .expect_err("a fully yanked crate cannot be a baseline")
            .to_string();
        assert!(failure.contains(CONTRACT_CRATE), "{failure}");
        assert!(failure.contains("not yanked"), "{failure}");
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
                     {\"name\":\"phoxal\",\"vers\":\"0.58.1\",\"yanked\":true,\
                     \"rust_version\":\"1.88\"}\n";
        let versions = PublishedVersion::read(entry).expect("the entry reads");
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[1].version, Version::new(0, 58, 1));
        assert!(versions[1].yanked);
        assert_eq!(versions[1].rust_version.as_deref(), Some("1.88"));
        assert_eq!(versions[0].rust_version, None);
    }
}
