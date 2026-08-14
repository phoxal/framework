//! Which published train the workspace is compared against.
//!
//! The published crates are the baseline. Nothing is stored in the repository
//! for a comparison to read: a snapshot committed beside the code would be
//! updated by the same change it is supposed to judge, so the registry - which
//! a release cannot rewrite - is the only honest record of what shipped.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;

use crate::surface::{CONTRACT_CRATES, ContractCrate};
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
/// The version and the toolchain floor come from the same index entries, so a
/// baseline is one fact rather than two that could describe different releases.
#[derive(Clone, Debug)]
pub(crate) struct PublishedTrain {
    /// The version all five contract crates share.
    pub(crate) version: Version,
    /// The toolchain floor they were published under, when they state one.
    pub(crate) rust_version: Option<RustVersion>,
    /// The actual registry package selected for each stable carrier.
    ///
    /// This is deliberately carried to the surface probe: resolving a legacy
    /// predecessor and then probing the new package name would compare a
    /// different train from the one whose completeness and floor were checked.
    selected_packages: BTreeMap<&'static str, &'static str>,
}

impl PublishedTrain {
    /// One train stated outright, by a drill that reads no registry.
    pub(crate) fn stated(version: Version, rust_version: Option<RustVersion>) -> Self {
        Self {
            version,
            rust_version,
            selected_packages: CONTRACT_CRATES
                .iter()
                .map(|contract_crate| {
                    (
                        contract_crate.carrier,
                        contract_crate
                            .predecessor_package
                            .unwrap_or(contract_crate.published_package),
                    )
                })
                .collect(),
        }
    }

    /// The package selected from registry history for `carrier`.
    pub(crate) fn selected_package(&self, carrier: &str) -> Result<&'static str> {
        self.selected_packages
            .get(carrier)
            .copied()
            .with_context(|| format!("published train selected no package for {carrier}"))
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

    /// Select the registry package that supplies one stable carrier.
    ///
    /// A predecessor is consulted only while the new package has no history at
    /// all. Any new-package entry, including a yanked one, permanently closes
    /// that fallback so a failed new line cannot silently compare against the
    /// old owner again.
    fn selected_history(
        &self,
        contract_crate: ContractCrate,
    ) -> Result<(&'static str, Vec<PublishedVersion>)> {
        let history = self.versions(contract_crate.published_package)?;
        if !history.is_empty() {
            return Ok((contract_crate.published_package, history));
        }
        let Some(predecessor) = contract_crate.predecessor_package else {
            return Ok((contract_crate.published_package, history));
        };
        Ok((predecessor, self.versions(predecessor)?))
    }

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
    ///
    /// The train's toolchain floor is read from the same entries. All five
    /// crates are built from one workspace and inherit one
    /// `[workspace.package] rust-version`, so a train that states two floors is
    /// not one train and is refused for the same reason a split version is.
    fn latest_train(&self) -> Result<PublishedTrain> {
        let mut latest = Vec::new();
        for contract_crate in CONTRACT_CRATES {
            let (selected_package, history) = self.selected_history(contract_crate)?;
            let newest = history
                .into_iter()
                .filter(|published| !published.yanked)
                .max_by(|left, right| left.version.cmp(&right.version))
                .with_context(|| {
                    format!(
                        "{} has no published version that is not yanked, so there is no train to \
                         compare against",
                        selected_package
                    )
                })?;
            latest.push((contract_crate.carrier, selected_package, newest));
        }

        let version = latest
            .iter()
            .map(|(_, _, published)| published.version.clone())
            .max()
            .context("no contract crate is declared, so no baseline can be resolved")?;
        if latest
            .iter()
            .any(|(_, _, published)| published.version != version)
        {
            let listed = latest
                .iter()
                .map(|(_, package, published)| format!("{package} {}", published.version))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "the published framework train is incomplete: the contract crates do not share a \
                 newest version ({listed}). A train publishes as one set, so comparing against a \
                 partially published train would compare one crate's contracts against another \
                 crate's previous release. Re-run once the publish finishes."
            );
        }

        let mut floors = Vec::new();
        for (_, package, published) in &latest {
            let floor = published
                .rust_version
                .as_deref()
                .map(RustVersion::parse)
                .transpose()
                .with_context(|| {
                    format!("{package} {version} states an unreadable rust-version in the registry")
                })?;
            floors.push((*package, floor));
        }
        let (_, stated) = floors
            .first()
            .cloned()
            .context("no contract crate is declared, so no toolchain floor can be resolved")?;
        if floors.iter().any(|(_, floor)| *floor != stated) {
            let listed = floors
                .iter()
                .map(|(name, floor)| match floor {
                    Some(floor) => format!("{name} {floor}"),
                    None => format!("{name} (none)"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "the published {version} train does not state one toolchain floor ({listed}). A \
                 train is built from one workspace and every crate inherits its `rust-version`, \
                 so two floors mean the published set is not one train and the floor it requires \
                 cannot be read."
            );
        }
        Ok(PublishedTrain {
            version,
            rust_version: stated,
            selected_packages: latest
                .into_iter()
                .map(|(carrier, package, _)| (carrier, package))
                .collect(),
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
            // with zero history. The rename resolver needs to distinguish that
            // state from a real read failure before consulting a predecessor.
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
        /// Every contract crate publishing the same versions.
        fn train(versions: &[(&str, bool)]) -> Self {
            let mut index = Self::default();
            for contract_crate in CONTRACT_CRATES {
                index.entries.insert(
                    contract_crate
                        .predecessor_package
                        .unwrap_or(contract_crate.published_package),
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

        /// One crate's newest release publishing under a different floor.
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
    fn the_newest_version_of_a_complete_train_is_the_baseline() {
        let index = FixtureIndex::train(&[("0.58.1", false), ("0.57.0", false), ("0.58.0", false)]);
        let train = index.latest_train().expect("a complete train resolves");
        assert_eq!(train.version, Version::new(0, 58, 1));
        assert_eq!(
            train
                .selected_package("phoxal-protocol")
                .expect("the protocol carrier resolves"),
            "phoxal-api"
        );
        assert_eq!(
            train.rust_version,
            Some(RustVersion::parse("1.88").expect("the floor parses"))
        );
    }

    /// The floor comes from the resolved train's own entries, so a newer
    /// release's floor is the baseline's floor and an older one's is not.
    #[test]
    fn the_baseline_floor_is_the_one_the_resolved_train_published_under() {
        let index = FixtureIndex::default()
            .with_floor(
                "phoxal",
                &[("0.58.0", Some("1.85")), ("0.58.1", Some("1.88"))],
            )
            .with_floor("phoxal-api", &[("0.58.1", Some("1.88"))])
            .with_floor("phoxal-bundle", &[("0.58.1", Some("1.88.0"))])
            .with_floor("phoxal-bus", &[("0.58.1", Some("1.88"))])
            .with_floor("phoxal-runtime-contract", &[("0.58.1", Some("1.88"))]);
        let train = index.latest_train().expect("a complete train resolves");
        assert_eq!(
            train.rust_version,
            Some(RustVersion::parse("1.88").expect("the floor parses"))
        );
    }

    /// A train older than the index field states no floor, which is an answer
    /// rather than a failure: it promises nothing about a toolchain.
    #[test]
    fn a_train_that_states_no_floor_resolves_without_one() {
        let index = FixtureIndex::default()
            .with_floor("phoxal", &[("0.50.0", None)])
            .with_floor("phoxal-api", &[("0.50.0", None)])
            .with_floor("phoxal-bundle", &[("0.50.0", None)])
            .with_floor("phoxal-bus", &[("0.50.0", None)])
            .with_floor("phoxal-runtime-contract", &[("0.50.0", None)]);
        let train = index.latest_train().expect("a complete train resolves");
        assert_eq!(train.version, Version::new(0, 50, 0));
        assert_eq!(train.rust_version, None);
    }

    /// One train is built from one workspace, so its crates share one floor.
    /// Two floors mean the set is not one train, and the checker says which
    /// crates disagree rather than picking one.
    #[test]
    fn a_train_whose_crates_state_two_floors_is_refused() {
        let index = FixtureIndex::default()
            .with_floor("phoxal", &[("0.58.1", Some("1.88"))])
            .with_floor("phoxal-api", &[("0.58.1", Some("1.90"))])
            .with_floor("phoxal-bundle", &[("0.58.1", Some("1.88"))])
            .with_floor("phoxal-bus", &[("0.58.1", None)])
            .with_floor("phoxal-runtime-contract", &[("0.58.1", Some("1.88"))]);
        let failure = index
            .latest_train()
            .expect_err("a split floor cannot be a baseline")
            .to_string();
        assert!(failure.contains("one toolchain floor"), "{failure}");
        assert!(failure.contains("phoxal-api 1.90"), "{failure}");
        assert!(failure.contains("phoxal-bus (none)"), "{failure}");
    }

    /// A yanked release cannot be resolved by anyone, so it cannot be the
    /// contract everyone else is holding.
    #[test]
    fn a_yanked_newest_version_falls_back_to_the_previous_one() {
        let index = FixtureIndex::train(&[("0.58.0", false), ("0.58.1", true)]);
        assert_eq!(
            index
                .latest_train()
                .expect("the previous train resolves")
                .version,
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

    /// The renamed carrier uses its predecessor only while the new package has
    /// no history at all, and records the actual package chosen for the probe.
    #[test]
    fn the_first_protocol_baseline_uses_the_predecessor_package() {
        let index = FixtureIndex::train(&[("0.58.1", false)]);
        let train = index
            .latest_train()
            .expect("the predecessor train resolves");
        assert_eq!(
            train
                .selected_package("phoxal-protocol")
                .expect("the protocol carrier resolves"),
            "phoxal-api"
        );
        let queries = index.queries.borrow();
        let protocol = queries
            .iter()
            .position(|name| name == "phoxal-protocol")
            .expect("the new package is queried");
        let api = queries
            .iter()
            .position(|name| name == "phoxal-api")
            .expect("the empty new history falls back");
        assert!(protocol < api, "queries were {queries:?}");
    }

    /// Once the renamed package exists it is the only protocol history read;
    /// the predecessor can no longer influence completeness or floors.
    #[test]
    fn a_published_protocol_disables_the_predecessor_fallback() {
        let index = FixtureIndex::train(&[("0.58.1", false)])
            .with("phoxal-protocol", &[("0.58.1", false)])
            .with("phoxal-api", &[("0.99.0", false)]);
        let train = index.latest_train().expect("the renamed train resolves");
        assert_eq!(
            train
                .selected_package("phoxal-protocol")
                .expect("the protocol carrier resolves"),
            "phoxal-protocol"
        );
        let queries = index.queries.borrow();
        assert!(queries.iter().any(|name| name == "phoxal-protocol"));
        assert!(
            !queries.iter().any(|name| name == "phoxal-api"),
            "queries were {queries:?}"
        );
    }

    /// The train floor comes from the actual selected renamed package, not a
    /// predecessor entry that happens to carry the same version.
    #[test]
    fn the_floor_comes_from_the_actual_selected_protocol_package() {
        let index = FixtureIndex::train(&[("0.58.1", false)])
            .with_floor("phoxal-protocol", &[("0.58.1", Some("1.90"))])
            .with_floor("phoxal-api", &[("0.58.1", Some("1.70"))])
            .with_floor("phoxal", &[("0.58.1", Some("1.90"))])
            .with_floor("phoxal-bundle", &[("0.58.1", Some("1.90"))])
            .with_floor("phoxal-bus", &[("0.58.1", Some("1.90"))])
            .with_floor("phoxal-runtime-contract", &[("0.58.1", Some("1.90"))]);
        let train = index.latest_train().expect("the renamed train resolves");
        assert_eq!(
            train.rust_version,
            Some(RustVersion::parse("1.90").expect("the floor parses"))
        );
        assert!(
            !index
                .queries
                .borrow()
                .iter()
                .any(|name| name == "phoxal-api")
        );
    }

    /// Even a yanked new-package entry permanently closes the predecessor
    /// fallback: returning to the old owner would hide a failed cutover.
    #[test]
    fn yanked_only_protocol_history_fails_without_reactivating_the_predecessor() {
        let index = FixtureIndex::train(&[("0.58.1", false)])
            .with("phoxal-protocol", &[("0.58.1", true)])
            .with("phoxal-api", &[("0.58.1", false)]);
        let failure = index
            .latest_train()
            .expect_err("a yanked-only renamed package cannot fall back")
            .to_string();
        assert!(failure.contains("phoxal-protocol"), "{failure}");
        let queries = index.queries.borrow();
        assert!(
            !queries.iter().any(|name| name == "phoxal-api"),
            "queries were {queries:?}"
        );
    }

    /// Completeness diagnostics name the actual selected package, not only the
    /// stable carrier that aliases it in the probe.
    #[test]
    fn completeness_uses_the_actual_selected_package_names() {
        let index =
            FixtureIndex::train(&[("0.58.1", false)]).with("phoxal-api", &[("0.59.0", false)]);
        let failure = index
            .latest_train()
            .expect_err("the selected predecessor splits the train")
            .to_string();
        assert!(failure.contains("phoxal-api 0.59.0"), "{failure}");
        assert!(!failure.contains("phoxal-protocol 0.59.0"), "{failure}");
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
