//! One comparison of the workspace against the latest published train.

use std::fmt;

use anyhow::{Result, bail};
use semver::Version;

use crate::index::PublishedVersions;
use crate::probe::{ContractSurfaces, Extraction, Side};
use crate::release::ReleaseStep;
use crate::surface::{CompatibilityImpact, RecordChange, SurfaceSet};

/// The compatibility check: resolve the published baseline, read both sides'
/// contract surfaces, and classify the difference.
pub(crate) struct CompatibilityCheck<V: PublishedVersions, S: ContractSurfaces> {
    versions: V,
    surfaces: S,
    workspace_version: Version,
}

impl<V: PublishedVersions, S: ContractSurfaces> CompatibilityCheck<V, S> {
    /// Compare `workspace_version`'s crates against whatever `versions` says is
    /// published.
    pub(crate) fn new(versions: V, surfaces: S, workspace_version: Version) -> Self {
        Self {
            versions,
            surfaces,
            workspace_version,
        }
    }

    /// Run the comparison, raising the mechanical impact to `declared`.
    ///
    /// The baseline is read first: when the published crates carry no surface
    /// there is nothing to compare against, and compiling the workspace side
    /// would answer a question nobody can ask yet.
    pub(crate) fn run(&self, declared: CompatibilityImpact) -> Result<CompatibilityReport> {
        let baseline = self.versions.latest_train()?;
        let Extraction::Surfaces(published) =
            self.surfaces.extract(&Side::Baseline(baseline.clone()))?
        else {
            return Ok(CompatibilityReport {
                baseline,
                workspace_version: self.workspace_version.clone(),
                outcome: Outcome::NoComparableBaseline,
            });
        };
        let Extraction::Surfaces(current) = self.surfaces.extract(&Side::Current)? else {
            bail!(
                "the workspace crates state no contract surface, so this workspace cannot be \
                 compared against anything"
            );
        };

        let changes = SurfaceSet::read(&published)?.changes_to(&SurfaceSet::read(&current)?);
        let mechanical = CompatibilityImpact::of(&changes);
        let effective = mechanical.max(declared);
        Ok(CompatibilityReport {
            workspace_version: self.workspace_version.clone(),
            outcome: Outcome::Compared {
                mechanical,
                declared,
                effective,
                required: ReleaseStep::required(effective, &baseline),
                changes,
            },
            baseline,
        })
    }
}

/// What one comparison found.
pub(crate) struct CompatibilityReport {
    baseline: Version,
    workspace_version: Version,
    outcome: Outcome,
}

impl CompatibilityReport {
    /// Why the workspace version is not a sufficient release over the
    /// baseline, or `None` when it is.
    ///
    /// A baseline with no surface to compare against constrains no release: the
    /// check has nothing to say about a train it cannot read.
    pub(crate) fn release_shortfall(&self) -> Option<InsufficientRelease> {
        let Outcome::Compared {
            effective,
            required,
            ..
        } = &self.outcome
        else {
            return None;
        };
        if required.is_satisfied_by(&self.baseline, &self.workspace_version) {
            return None;
        }
        Some(InsufficientRelease {
            workspace_version: self.workspace_version.clone(),
            baseline: self.baseline.clone(),
            impact: *effective,
            minimum: required.minimum_version(&self.baseline),
        })
    }
}

impl fmt::Display for CompatibilityReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "baseline:  {} (latest published framework train)",
            self.baseline
        )?;
        writeln!(formatter, "workspace: {}", self.workspace_version)?;
        let Outcome::Compared {
            mechanical,
            declared,
            effective,
            required,
            changes,
        } = &self.outcome
        else {
            return write!(
                formatter,
                "no comparable baseline: the published {} crates state no contract surface, so \
                 there is nothing to compare. The first release that carries one becomes the \
                 first comparable baseline.",
                self.baseline
            );
        };

        write!(formatter, "impact:    {effective}")?;
        if declared > mechanical {
            write!(
                formatter,
                " (declared; the surfaces themselves are {mechanical})"
            )?;
        }
        writeln!(formatter)?;
        writeln!(
            formatter,
            "release:   at least a {required} ({} or higher)",
            required.minimum_version(&self.baseline)
        )?;
        if changes.is_empty() {
            return write!(formatter, "contracts: unchanged");
        }
        write!(formatter, "contracts:")?;
        for change in changes {
            write!(formatter, "\n  {change}")?;
        }
        Ok(())
    }
}

/// What the comparison could establish.
enum Outcome {
    /// The published crates state no contract surface.
    NoComparableBaseline,
    /// Both sides stated their surfaces and the difference was classified.
    Compared {
        /// What the surfaces themselves show.
        mechanical: CompatibilityImpact,
        /// What the caller declared on top of them.
        declared: CompatibilityImpact,
        /// The greater of the two, which is what the release must carry.
        effective: CompatibilityImpact,
        /// The smallest release that may carry it.
        required: ReleaseStep,
        /// The records behind the impact.
        changes: Vec<RecordChange>,
    },
}

/// The workspace version does not clear the release its own contract changes
/// require.
pub(crate) struct InsufficientRelease {
    workspace_version: Version,
    baseline: Version,
    impact: CompatibilityImpact,
    minimum: Version,
}

impl fmt::Display for InsufficientRelease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the workspace is at {} over a published {}, which is not enough for a {} contract \
             change: release {} or higher",
            self.workspace_version, self.baseline, self.impact, self.minimum
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::*;
    use crate::index::PublishedVersion;
    use crate::surface::CONTRACT_CRATES;

    /// A registry that publishes one complete train.
    struct FixtureTrain(Version);

    impl PublishedVersions for FixtureTrain {
        fn versions(&self, _crate_name: &str) -> Result<Vec<PublishedVersion>> {
            Ok(vec![PublishedVersion::published(&self.0)])
        }
    }

    /// Surfaces a test states outright, with no compilation.
    struct FixtureSurfaces {
        baseline: Extraction,
        current: Extraction,
    }

    impl FixtureSurfaces {
        /// One record set per side, as the contract crates would state them.
        fn new(baseline: &[Value], current: &[Value]) -> Self {
            Self {
                baseline: Extraction::Surfaces(documents(baseline)),
                current: Extraction::Surfaces(documents(current)),
            }
        }

        /// A published train from before the contract surfaces existed.
        fn without_a_baseline_surface(current: &[Value]) -> Self {
            Self {
                baseline: Extraction::NoContractSurface,
                current: Extraction::Surfaces(documents(current)),
            }
        }
    }

    impl ContractSurfaces for FixtureSurfaces {
        fn extract(&self, side: &Side) -> Result<Extraction> {
            Ok(match side {
                Side::Baseline(_) => self.baseline.clone(),
                Side::Current => self.current.clone(),
            })
        }
    }

    /// Put every record on `phoxal-api` and leave the other contract crates
    /// declaring nothing, which is a surface set the comparison accepts.
    fn documents(records: &[Value]) -> BTreeMap<String, Value> {
        CONTRACT_CRATES
            .iter()
            .map(|contract_crate| {
                let declared = if contract_crate.name == "phoxal-api" {
                    records.to_vec()
                } else {
                    Vec::new()
                };
                (
                    contract_crate.name.to_owned(),
                    json!({ "records": declared }),
                )
            })
            .collect()
    }

    fn endpoint(path: &str) -> Value {
        json!({
            "delivery": "state",
            "family": "robot",
            "kind": "state",
            "path": path,
            "payload": {"fields": [], "kind": "struct"},
            "record": "endpoint",
            "request": Value::Null,
            "response": Value::Null,
        })
    }

    fn check(
        baseline: Version,
        workspace: Version,
        surfaces: FixtureSurfaces,
    ) -> CompatibilityCheck<FixtureTrain, FixtureSurfaces> {
        CompatibilityCheck::new(FixtureTrain(baseline), surfaces, workspace)
    }

    /// An unchanged surface leaves the release free, so a workspace sitting one
    /// patch above the published train passes.
    #[test]
    fn an_unchanged_workspace_needs_only_a_patch() {
        let report = check(
            Version::new(0, 58, 1),
            Version::new(0, 58, 2),
            FixtureSurfaces::new(&[endpoint("robot/a")], &[endpoint("robot/a")]),
        )
        .run(CompatibilityImpact::Unchanged)
        .expect("the comparison runs");
        assert!(report.release_shortfall().is_none());
        assert!(report.to_string().contains("impact:    unchanged"));
        assert!(report.to_string().contains("contracts: unchanged"));
    }

    /// The version gate is the point of `check-release`: an addition on a patch
    /// bump is refused, the next minor passes, and over-releasing stays
    /// allowed.
    #[test]
    fn an_additive_change_refuses_a_patch_and_accepts_the_next_minor() {
        let surfaces = || {
            FixtureSurfaces::new(
                &[endpoint("robot/a")],
                &[endpoint("robot/a"), endpoint("robot/b")],
            )
        };
        let shortfall = check(Version::new(0, 58, 1), Version::new(0, 58, 2), surfaces())
            .run(CompatibilityImpact::Unchanged)
            .expect("the comparison runs")
            .release_shortfall()
            .expect("a patch cannot carry an addition")
            .to_string();
        assert!(shortfall.contains("0.59.0"), "{shortfall}");
        assert!(shortfall.contains("additive"), "{shortfall}");

        for accepted in [Version::new(0, 59, 0), Version::new(0, 60, 0)] {
            assert!(
                check(Version::new(0, 58, 1), accepted.clone(), surfaces())
                    .run(CompatibilityImpact::Unchanged)
                    .expect("the comparison runs")
                    .release_shortfall()
                    .is_none(),
                "{accepted} is at least the next minor"
            );
        }
    }

    /// From 1.0 on, a break needs the major it is a break of.
    #[test]
    fn a_break_after_one_point_zero_needs_a_major() {
        let report = check(
            Version::new(1, 4, 2),
            Version::new(1, 5, 0),
            FixtureSurfaces::new(&[endpoint("robot/a")], &[]),
        )
        .run(CompatibilityImpact::Unchanged)
        .expect("the comparison runs");
        assert!(report.to_string().contains("impact:    breaking"));
        assert!(report.to_string().contains("at least a major (2.0.0"));
        let shortfall = report
            .release_shortfall()
            .expect("a minor cannot carry a break after 1.0")
            .to_string();
        assert!(shortfall.contains("2.0.0"), "{shortfall}");
    }

    /// A semantic break the surfaces cannot show is declared, and the declared
    /// level raises the release requirement with it.
    #[test]
    fn a_declared_impact_raises_the_requirement() {
        let report = check(
            Version::new(0, 58, 1),
            Version::new(0, 58, 2),
            FixtureSurfaces::new(&[endpoint("robot/a")], &[endpoint("robot/a")]),
        )
        .run(CompatibilityImpact::Breaking)
        .expect("the comparison runs");
        let rendered = report.to_string();
        assert!(rendered.contains("impact:    breaking"), "{rendered}");
        assert!(
            rendered.contains("the surfaces themselves are unchanged"),
            "{rendered}"
        );
        assert!(report.release_shortfall().is_some());
    }

    /// The declaration only ever raises: nothing may talk a break the surfaces
    /// show down to an addition.
    #[test]
    fn a_declared_impact_cannot_lower_a_mechanical_one() {
        let report = check(
            Version::new(0, 58, 1),
            Version::new(0, 58, 2),
            FixtureSurfaces::new(&[endpoint("robot/a")], &[]),
        )
        .run(CompatibilityImpact::Unchanged)
        .expect("the comparison runs");
        assert!(report.to_string().contains("impact:    breaking"));
        assert!(report.release_shortfall().is_some());
    }

    /// Until a surface-carrying train is published there is nothing to compare
    /// against, and that is reported outright rather than passing as an
    /// unchanged surface.
    #[test]
    fn a_baseline_without_surfaces_is_reported_and_gates_nothing() {
        let report = check(
            Version::new(0, 58, 1),
            Version::new(0, 58, 1),
            FixtureSurfaces::without_a_baseline_surface(&[endpoint("robot/a")]),
        )
        .run(CompatibilityImpact::Breaking)
        .expect("the comparison runs");
        let rendered = report.to_string();
        assert!(rendered.contains("no comparable baseline"), "{rendered}");
        assert!(rendered.contains("0.58.1"), "{rendered}");
        assert!(report.release_shortfall().is_none());
    }
}
