//! One comparison of the workspace against the latest published train.

use std::fmt;

use anyhow::{Result, bail};
use semver::Version;

use crate::index::PublishedVersions;
use crate::probe::{ContractSurfaces, Extraction, Side};
use crate::release::ReleaseStep;
use crate::runbook::{RunbookPointer, RunbookRule};
use crate::source::{AuthoredDocuments, CorpusReading, SourceComparison};
use crate::surface::{CompatibilityImpact, RecordChange, SurfaceSet};
use crate::toolchain::{RustVersion, ToolchainFloor};

/// The compatibility check: resolve the published baseline, read both sides'
/// contract surfaces, authored-source readings and toolchain floors, and
/// classify the difference.
pub(crate) struct CompatibilityCheck<
    V: PublishedVersions,
    S: ContractSurfaces,
    A: AuthoredDocuments,
> {
    versions: V,
    surfaces: S,
    documents: A,
    workspace_version: Version,
    workspace_floor: RustVersion,
}

impl<V: PublishedVersions, S: ContractSurfaces, A: AuthoredDocuments> CompatibilityCheck<V, S, A> {
    /// Compare `workspace_version`'s crates against whatever `versions` says is
    /// published.
    pub(crate) fn new(
        versions: V,
        surfaces: S,
        documents: A,
        workspace_version: Version,
        workspace_floor: RustVersion,
    ) -> Self {
        Self {
            versions,
            surfaces,
            documents,
            workspace_version,
            workspace_floor,
        }
    }

    /// Run the comparison, raising the mechanical impact to `declared`.
    ///
    /// The authored source is read first, because it is the one axis that
    /// speaks whatever the contract surfaces do: the authored-source layer is
    /// not a wire surface, so a published reader is comparable even on a train
    /// that states no surface at all - as long as that reader carries the probe
    /// entry the corpus is read through. One that predates it leaves the leg
    /// unavailable rather than failing the run; a workspace that dropped the
    /// entry is a defect in this repository and does fail.
    ///
    /// The contract baseline is read next: when the published crates carry no
    /// surface there is nothing to compare against, and compiling the workspace
    /// side would answer a question nobody can ask yet. The toolchain floor is
    /// read from the same index entry the baseline came from, so it is compared
    /// even on a train whose surfaces cannot be.
    pub(crate) fn run(&self, declared: CompatibilityImpact) -> Result<CompatibilityReport> {
        let train = self.versions.latest_train()?;
        let baseline = train.version.clone();
        let floor =
            ToolchainFloor::between(train.rust_version.clone(), self.workspace_floor.clone());
        let source = self.compare_authored_source(&train)?;

        let Extraction::Surfaces(published) = self.surfaces.extract(&Side::Baseline(train))? else {
            return Ok(CompatibilityReport {
                baseline,
                workspace_version: self.workspace_version.clone(),
                floor,
                source,
                declared,
                outcome: Outcome::NoComparableBaseline,
            });
        };
        let Extraction::Surfaces(current) = self.surfaces.extract(&Side::Current)? else {
            bail!(
                "the workspace framework library states no contract surface, so this workspace \
                 cannot be compared against anything"
            );
        };

        let changes = SurfaceSet::read(&published)?.changes_to(&SurfaceSet::read(&current)?);
        let mechanical = CompatibilityImpact::of(&changes);
        Ok(CompatibilityReport {
            workspace_version: self.workspace_version.clone(),
            floor,
            outcome: Outcome::Compared {
                effective: mechanical.max(declared).max(source.impact()),
                mechanical,
                changes,
            },
            source,
            declared,
            baseline,
        })
    }

    /// Read the corpus through both readers and classify the difference.
    ///
    /// A published reader with no probe entry is the one side whose absence is
    /// an outcome: the entry is younger than that train, and no version of it
    /// can be made to answer. The workspace side has no such excuse - the entry
    /// is in the tree in front of it - so a workspace that cannot be read is an
    /// error naming what went missing.
    fn compare_authored_source(
        &self,
        train: &crate::index::PublishedTrain,
    ) -> Result<SourceComparison> {
        let baseline = self.documents.read(&Side::Baseline(train.clone()))?;
        let CorpusReading::Corpus(current) = self.documents.read(&Side::Current)? else {
            bail!(
                "the workspace's `phoxal::authoring` states no compatibility probe entry, so this \
                 workspace's authored source cannot be read at all"
            );
        };
        let CorpusReading::Corpus(baseline) = baseline else {
            return Ok(SourceComparison::Unavailable);
        };
        SourceComparison::between(&baseline, &current)
    }
}

/// What one comparison found.
pub(crate) struct CompatibilityReport {
    baseline: Version,
    workspace_version: Version,
    floor: ToolchainFloor,
    source: SourceComparison,
    /// What the caller declared on top of whatever the axes mechanically show.
    ///
    /// It lives on the report rather than inside [`Outcome::Compared`] because
    /// an axis that could not be compared at all still has to know it: an
    /// unreadable authored-source baseline is tolerated only on a train that is
    /// already declaring a break.
    declared: CompatibilityImpact,
    outcome: Outcome,
}

impl CompatibilityReport {
    /// Every reason this candidate is constrained at all, in the order a
    /// reader meets them.
    ///
    /// A baseline with no surface to compare against contributes no contract
    /// requirement: the check has nothing to say about contracts it cannot
    /// read. The authored source and the toolchain floor are separate axes,
    /// read from the published reader and from the registry rather than from a
    /// surface, so both still speak on such a train.
    fn requirements(&self) -> Vec<ReleaseRequirement> {
        let mut requirements = Vec::new();
        if let Outcome::Compared {
            mechanical,
            changes,
            ..
        } = &self.outcome
        {
            let impact = (*mechanical).max(self.declared);
            requirements.push(ReleaseRequirement::Contracts {
                impact,
                step: ReleaseStep::required(impact, &self.baseline),
                frozen: changes
                    .iter()
                    .any(RecordChange::breaks_the_frozen_bootstrap),
            });
        }
        let source = self.source.impact();
        if source > CompatibilityImpact::Unchanged {
            requirements.push(ReleaseRequirement::AuthoredSource {
                impact: source,
                step: ReleaseStep::required(source, &self.baseline),
            });
        }
        if let Some(step) = self.floor.required_step() {
            requirements.push(ReleaseRequirement::ToolchainFloor {
                floor: self.floor.clone(),
                step,
            });
        }
        requirements
    }

    /// The smallest release this candidate may be, over all axes.
    fn binding_step(&self) -> Option<ReleaseStep> {
        self.requirements()
            .iter()
            .map(ReleaseRequirement::step)
            .max()
    }

    /// The release this candidate is being sized as, over every axis that could
    /// be read.
    fn effective_impact(&self) -> CompatibilityImpact {
        match &self.outcome {
            Outcome::Compared { effective, .. } => *effective,
            Outcome::NoComparableBaseline => self.declared.max(self.source.impact()),
        }
    }

    /// The failure one command must return, if any.
    ///
    /// Frozen bootstrap drift is unreleasable and therefore fails both the
    /// ordinary report and the release gate. Every other release-size finding
    /// remains report-only until `check-release` asks to gate the candidate.
    ///
    /// An unavailable authored-source leg is checked before the release size,
    /// because it is a precondition rather than a version: no candidate is big
    /// enough to buy a train that nothing held its source language to, and
    /// naming the missing leg first is what tells the reader that raising the
    /// version will not answer it.
    pub(crate) fn command_failure(&self, gates_the_release: bool) -> Option<CompatibilityFailure> {
        if self.requirements().iter().any(|requirement| {
            matches!(
                requirement,
                ReleaseRequirement::Contracts { frozen: true, .. }
            )
        }) {
            return Some(CompatibilityFailure::FrozenBootstrapDrift);
        }
        if !gates_the_release {
            return None;
        }
        if self.source.is_unavailable() && self.effective_impact() != CompatibilityImpact::Breaking
        {
            return Some(CompatibilityFailure::UnreadableSourceBaseline);
        }
        self.release_shortfall()
            .map(CompatibilityFailure::InsufficientRelease)
    }

    /// The authored-source line: one word when the whole corpus came through
    /// unchanged, and the corpus itself when it did not.
    fn write_source(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.source.is_clean() {
            return writeln!(formatter, "source:    {}", self.source);
        }
        writeln!(formatter, "source:{}", self.source)
    }

    /// Why the workspace version is not a sufficient release over the
    /// baseline, or `None` when it is.
    pub(crate) fn release_shortfall(&self) -> Option<InsufficientRelease> {
        let binding = self.binding_step()?;
        if binding.is_satisfied_by(&self.baseline, &self.workspace_version) {
            return None;
        }
        Some(InsufficientRelease {
            workspace_version: self.workspace_version.clone(),
            baseline: self.baseline.clone(),
            minimum: binding.minimum_version(&self.baseline),
            // Only the axes that actually bind: an axis a smaller release would
            // already satisfy is not why this candidate was refused.
            binding: self
                .requirements()
                .into_iter()
                .filter(|requirement| requirement.step() == binding)
                .collect(),
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
        writeln!(formatter, "toolchain: {}", self.floor)?;
        let Outcome::Compared {
            mechanical,
            effective,
            changes,
        } = &self.outcome
        else {
            self.write_source(formatter)?;
            return write!(
                formatter,
                "no comparable baseline: the published {} crates state no contract surface, so \
                 there is nothing to compare. The first release that carries one becomes the \
                 first comparable baseline.",
                self.baseline
            );
        };

        write!(formatter, "impact:    {effective}")?;
        // Everything the surfaces alone do not account for is named, so a
        // reader is never left guessing which axis raised the release.
        let raised = [
            (self.declared > *mechanical, "declared"),
            (self.source.impact() > *mechanical, "the authored source"),
        ]
        .into_iter()
        .filter_map(|(applies, reason)| applies.then_some(reason))
        .collect::<Vec<_>>();
        if !raised.is_empty() {
            write!(
                formatter,
                " ({}; the surfaces themselves are {mechanical})",
                raised.join(" and ")
            )?;
        }
        writeln!(formatter)?;
        if let Some(binding) = self.binding_step() {
            writeln!(
                formatter,
                "release:   at least a {binding} ({} or higher)",
                binding.minimum_version(&self.baseline)
            )?;
        }
        self.write_source(formatter)?;
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

/// One axis that constrains how small this release may be.
#[derive(Clone, Debug)]
enum ReleaseRequirement {
    /// What the contract surfaces changed.
    Contracts {
        impact: CompatibilityImpact,
        step: ReleaseStep,
        /// Whether a record the attachment bootstrap freezes is among them.
        frozen: bool,
    },
    /// What this workspace's reader did to the documents the published reader
    /// already accepted.
    AuthoredSource {
        impact: CompatibilityImpact,
        step: ReleaseStep,
    },
    /// What the toolchain floor did.
    ToolchainFloor {
        floor: ToolchainFloor,
        step: ReleaseStep,
    },
}

impl ReleaseRequirement {
    fn step(&self) -> ReleaseStep {
        match self {
            Self::Contracts { step, .. }
            | Self::AuthoredSource { step, .. }
            | Self::ToolchainFloor { step, .. } => *step,
        }
    }

    /// The rule that says what may be done about this requirement.
    fn rule(&self) -> RunbookRule {
        match self {
            Self::Contracts { frozen: true, .. } => RunbookRule::FrozenBootstrapDrift,
            Self::Contracts {
                impact: CompatibilityImpact::Breaking,
                ..
            } => RunbookRule::BreakingContractChange,
            Self::AuthoredSource {
                impact: CompatibilityImpact::Breaking,
                ..
            } => RunbookRule::AuthoredSourceBreak,
            Self::Contracts { .. } | Self::AuthoredSource { .. } => {
                RunbookRule::UnderSizedCandidate
            }
            Self::ToolchainFloor { .. } => RunbookRule::ToolchainFloorRaised,
        }
    }
}

impl fmt::Display for ReleaseRequirement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contracts {
                impact: CompatibilityImpact::Unchanged,
                step,
                ..
            } => write!(
                formatter,
                "the contracts are unchanged, and a release is still at least a {step} over the \
                 baseline"
            ),
            Self::Contracts {
                impact,
                step,
                frozen: true,
            } => write!(
                formatter,
                "the contract change is {impact} at the FROZEN BOOTSTRAP: requires at least a \
                 {step} release"
            ),
            Self::Contracts { impact, step, .. } => write!(
                formatter,
                "the contract change is {impact}: requires at least a {step} release"
            ),
            Self::AuthoredSource {
                impact: CompatibilityImpact::Breaking,
                step,
            } => write!(
                formatter,
                "an authored document the published reader accepted is rejected or means \
                 something else: requires at least a {step} release"
            ),
            Self::AuthoredSource { impact, step } => write!(
                formatter,
                "the authored source language is {impact}: requires at least a {step} release"
            ),
            Self::ToolchainFloor {
                floor:
                    ToolchainFloor::Raised {
                        baseline,
                        workspace,
                    },
                step,
            } => write!(
                formatter,
                "rust-version floor raised {baseline} -> {workspace}: requires at least a {step} \
                 release"
            ),
            Self::ToolchainFloor { floor, step } => write!(
                formatter,
                "the toolchain floor ({floor}) requires at least a {step} release"
            ),
        }
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
        /// The greatest of the surfaces, the declaration and the authored
        /// source, which is what the release must carry.
        effective: CompatibilityImpact,
        /// The records behind the impact.
        changes: Vec<RecordChange>,
    },
}

/// The workspace version does not clear the release its own changes require.
pub(crate) struct InsufficientRelease {
    workspace_version: Version,
    baseline: Version,
    minimum: Version,
    binding: Vec<ReleaseRequirement>,
}

/// Why a compatibility command must fail after printing its complete report.
pub(crate) enum CompatibilityFailure {
    /// The frozen attachment bootstrap changed; no version can make it valid.
    FrozenBootstrapDrift,
    /// The published reader carries no probe entry, so the authored-source leg
    /// did not run, on a candidate that is not itself a breaking release.
    UnreadableSourceBaseline,
    /// `check-release` found a candidate smaller than the required release.
    InsufficientRelease(InsufficientRelease),
}

impl fmt::Display for CompatibilityFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrozenBootstrapDrift => write!(
                formatter,
                "the frozen supervisor bootstrap drifted and is unreleasable at any version. {}",
                RunbookPointer::to([RunbookRule::FrozenBootstrapDrift])
            ),
            Self::UnreadableSourceBaseline => write!(
                formatter,
                "the authored-source comparison is unavailable and this candidate is not a \
                 breaking release: the published reader has no probe entry, so nothing holds this \
                 train's source language to what the published one accepted. Only a train already \
                 declaring a break may release over an unread corpus. {}",
                RunbookPointer::to([RunbookRule::UnreadableSourceBaseline])
            ),
            Self::InsufficientRelease(shortfall) => shortfall.fmt(formatter),
        }
    }
}

impl fmt::Display for InsufficientRelease {
    /// The refusal, the axes behind it, and the one line that says what may be
    /// done about it. The pointer goes last so an agent reading a truncated CI
    /// log finds the decision tree at the end of the failure.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "the workspace is at {} over a published {}, which is not a sufficient release: \
             release {} or higher",
            self.workspace_version, self.baseline, self.minimum
        )?;
        for requirement in &self.binding {
            writeln!(formatter, "  {requirement}")?;
        }
        write!(
            formatter,
            "{}",
            RunbookPointer::to(self.binding.iter().map(ReleaseRequirement::rule))
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::index::PublishedVersion;
    use crate::source::Reading;

    /// The floor both sides carry unless a test is about the floor.
    const FLOOR: &str = "1.88";

    fn floor(value: &str) -> RustVersion {
        RustVersion::parse(value).expect("the floor parses")
    }

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

    /// The one document a side states, holding the records the test is about.
    fn documents(records: &[Value]) -> Vec<Value> {
        vec![json!({ "records": records })]
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

    /// What each side made of one authored document, stated outright.
    #[derive(Clone, Copy)]
    struct FixtureDocuments {
        current_accepts: bool,
        /// Whether the published reader carries the probe entry at all.
        baseline_reads: bool,
    }

    impl FixtureDocuments {
        /// A corpus that came through this workspace's reader unchanged, which
        /// is what every test not about the source axis needs.
        const fn unchanged() -> Self {
            Self {
                current_accepts: true,
                baseline_reads: true,
            }
        }

        /// A corpus this workspace's reader no longer accepts.
        const fn regressed() -> Self {
            Self {
                current_accepts: false,
                baseline_reads: true,
            }
        }

        /// A published train from before the probe entry existed.
        const fn without_a_baseline_reader() -> Self {
            Self {
                current_accepts: true,
                baseline_reads: false,
            }
        }
    }

    impl AuthoredDocuments for FixtureDocuments {
        fn read(&self, side: &Side) -> Result<CorpusReading> {
            if matches!(side, Side::Baseline(_)) && !self.baseline_reads {
                return Ok(CorpusReading::NoProbeEntry);
            }
            let accepted = Reading::Accepted {
                canonical: json!({"id": "r"}),
            };
            let reading = if matches!(side, Side::Current) && !self.current_accepts {
                Reading::Rejected {
                    error: "unknown field `mount_link`".to_owned(),
                }
            } else {
                accepted
            };
            Ok(CorpusReading::Corpus(
                [("robot.yaml", reading)].into_iter().collect(),
            ))
        }
    }

    fn check(
        baseline: Version,
        workspace: Version,
        surfaces: FixtureSurfaces,
    ) -> CompatibilityCheck<FixtureTrain, FixtureSurfaces, FixtureDocuments> {
        check_at_floor(baseline, workspace, surfaces, FLOOR)
    }

    fn check_at_floor(
        baseline: Version,
        workspace: Version,
        surfaces: FixtureSurfaces,
        workspace_floor: &str,
    ) -> CompatibilityCheck<FixtureTrain, FixtureSurfaces, FixtureDocuments> {
        CompatibilityCheck::new(
            FixtureTrain(baseline),
            surfaces,
            FixtureDocuments::unchanged(),
            workspace,
            floor(workspace_floor),
        )
    }

    /// The source axis on its own: nothing about the wire surfaces moved, and a
    /// document the published reader accepted no longer compiles. Pre-1.0 that
    /// is the next minor, and the refusal names the fixture, the reader's own
    /// complaint, and the rule that answers it.
    #[test]
    fn a_source_break_gates_a_release_on_its_own() {
        let report = CompatibilityCheck::new(
            FixtureTrain(Version::new(0, 58, 1)),
            FixtureSurfaces::new(&[endpoint("robot/a")], &[endpoint("robot/a")]),
            FixtureDocuments::regressed(),
            Version::new(0, 58, 2),
            floor(FLOOR),
        )
        .run(CompatibilityImpact::Unchanged)
        .expect("the comparison runs");
        let rendered = report.to_string();
        assert!(
            rendered.contains(
                "impact:    breaking (the authored source; the surfaces themselves are unchanged)"
            ),
            "{rendered}"
        );
        assert!(rendered.contains("contracts: unchanged"), "{rendered}");
        assert!(rendered.contains("regressed     robot.yaml"), "{rendered}");
        assert!(rendered.contains("[SOURCE-BREAKING]"), "{rendered}");
        assert!(
            rendered.contains("unknown field `mount_link`"),
            "{rendered}"
        );
        let shortfall = report
            .release_shortfall()
            .expect("a patch cannot carry a source break")
            .to_string();
        assert!(
            shortfall.contains("rejected or means something else"),
            "{shortfall}"
        );
        assert!(shortfall.contains("0.59.0"), "{shortfall}");
        assert!(shortfall.contains("rule 8"), "{shortfall}");
        assert!(!shortfall.contains("rules 1 and 2"), "{shortfall}");
    }

    /// A clean corpus says so in one line rather than listing itself, and it is
    /// reported even on a train whose contract surfaces cannot be compared.
    #[test]
    fn a_clean_corpus_is_reported_on_every_train() {
        for surfaces in [
            FixtureSurfaces::new(&[endpoint("robot/a")], &[endpoint("robot/a")]),
            FixtureSurfaces::without_a_baseline_surface(&[endpoint("robot/a")]),
        ] {
            let report = check(Version::new(0, 58, 1), Version::new(0, 58, 2), surfaces)
                .run(CompatibilityImpact::Unchanged)
                .expect("the comparison runs");
            assert!(
                report
                    .to_string()
                    .contains("source:    compatible (1 authored project)"),
                "{report}"
            );
        }
    }

    /// A published reader from before the probe entry existed leaves the leg
    /// with nothing to compare against. That is an informational line rather
    /// than a finding: `report` prints it and passes, and it raises no release
    /// requirement of its own.
    #[test]
    fn a_baseline_without_a_probe_entry_is_reported_and_passes_report() {
        let report = CompatibilityCheck::new(
            FixtureTrain(Version::new(0, 62, 1)),
            FixtureSurfaces::new(&[endpoint("robot/a")], &[endpoint("robot/a")]),
            FixtureDocuments::without_a_baseline_reader(),
            Version::new(0, 62, 2),
            floor(FLOOR),
        )
        .run(CompatibilityImpact::Unchanged)
        .expect("the comparison runs");
        let rendered = report.to_string();
        assert!(rendered.contains("source:    unavailable ("), "{rendered}");
        assert!(rendered.contains("no probe entry"), "{rendered}");
        assert!(rendered.contains("impact:    unchanged"), "{rendered}");
        assert!(report.release_shortfall().is_none(), "{rendered}");
        assert!(report.command_failure(false).is_none(), "{rendered}");
    }

    /// The allowance is exactly one train wide: `check-release` takes an
    /// unreadable source baseline only on a candidate that is already declaring
    /// a break, and refuses it on any smaller one. A bigger version is not the
    /// remedy, so the refusal names the missing reader rather than a minimum.
    #[test]
    fn an_unreadable_source_baseline_gates_every_train_that_is_not_breaking() {
        let check = |declared| {
            CompatibilityCheck::new(
                FixtureTrain(Version::new(0, 62, 1)),
                FixtureSurfaces::new(&[endpoint("robot/a")], &[endpoint("robot/a")]),
                FixtureDocuments::without_a_baseline_reader(),
                Version::new(0, 63, 0),
                floor(FLOOR),
            )
            .run(declared)
            .expect("the comparison runs")
        };

        for tolerated in [
            CompatibilityImpact::Unchanged,
            CompatibilityImpact::Additive,
        ] {
            let failure = check(tolerated)
                .command_failure(true)
                .expect("only a breaking train may release over an unread corpus")
                .to_string();
            assert!(failure.contains("no probe entry"), "{tolerated}: {failure}");
            assert!(
                failure.contains("not a breaking release"),
                "{tolerated}: {failure}"
            );
            assert!(failure.contains("rule 9"), "{tolerated}: {failure}");
        }

        let breaking = check(CompatibilityImpact::Breaking);
        assert!(
            breaking.command_failure(true).is_none(),
            "a declared break carries the allowance: {breaking}"
        );
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
        assert!(
            report
                .to_string()
                .contains("toolchain: 1.88 (unchanged from the published train)"),
            "{report}"
        );
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

    /// A raised floor breaks every consumer still on the previous toolchain, so
    /// it takes a minor of its own even when no contract moved at all.
    #[test]
    fn a_raised_toolchain_floor_requires_a_minor_on_its_own() {
        let report = check_at_floor(
            Version::new(0, 58, 1),
            Version::new(0, 58, 2),
            FixtureSurfaces::new(&[endpoint("robot/a")], &[endpoint("robot/a")]),
            "1.90",
        )
        .run(CompatibilityImpact::Unchanged)
        .expect("the comparison runs");
        let rendered = report.to_string();
        assert!(rendered.contains("impact:    unchanged"), "{rendered}");
        assert!(
            rendered.contains("toolchain: 1.90 (raised from the published 1.88"),
            "{rendered}"
        );
        assert!(
            rendered.contains("release:   at least a minor (0.59.0"),
            "{rendered}"
        );
        let shortfall = report
            .release_shortfall()
            .expect("a patch cannot carry a raised floor")
            .to_string();
        assert!(
            shortfall.contains("rust-version floor raised 1.88 -> 1.90"),
            "{shortfall}"
        );
        assert!(shortfall.contains("0.59.0"), "{shortfall}");
        assert!(shortfall.trim_end().ends_with("rule 4 (the toolchain floor rose - revert the raise or acknowledge it and release a minor)"), "{shortfall}");
    }

    /// An equal or lowered floor asks nothing of anybody, so it constrains no
    /// release and says so without complaint.
    #[test]
    fn an_equal_or_lowered_toolchain_floor_constrains_nothing() {
        for (workspace_floor, expected) in [("1.88", "unchanged"), ("1.87", "lowered")] {
            let report = check_at_floor(
                Version::new(0, 58, 1),
                Version::new(0, 58, 2),
                FixtureSurfaces::new(&[endpoint("robot/a")], &[endpoint("robot/a")]),
                workspace_floor,
            )
            .run(CompatibilityImpact::Unchanged)
            .expect("the comparison runs");
            assert!(report.to_string().contains(expected), "{report}");
            assert!(report.release_shortfall().is_none(), "{report}");
        }
    }

    /// Two axes can bind one candidate. The refusal names both, because fixing
    /// one and re-running to meet the other is a wasted cycle.
    #[test]
    fn a_refusal_names_every_axis_that_binds_it() {
        let shortfall = check_at_floor(
            Version::new(0, 58, 1),
            Version::new(0, 58, 2),
            FixtureSurfaces::new(
                &[endpoint("robot/a")],
                &[endpoint("robot/a"), endpoint("robot/b")],
            ),
            "1.90",
        )
        .run(CompatibilityImpact::Unchanged)
        .expect("the comparison runs")
        .release_shortfall()
        .expect("a patch carries neither an addition nor a raised floor")
        .to_string();
        assert!(
            shortfall.contains("contract change is additive"),
            "{shortfall}"
        );
        assert!(shortfall.contains("floor raised"), "{shortfall}");
        assert!(shortfall.contains("rule 4"), "{shortfall}");
        assert!(shortfall.contains("rule 7"), "{shortfall}");
    }

    /// The floor is read from the registry rather than from a surface, so it
    /// still gates a train whose contracts cannot be compared at all.
    #[test]
    fn a_raised_floor_gates_even_without_a_comparable_baseline() {
        let report = check_at_floor(
            Version::new(0, 58, 1),
            Version::new(0, 58, 2),
            FixtureSurfaces::without_a_baseline_surface(&[endpoint("robot/a")]),
            "1.90",
        )
        .run(CompatibilityImpact::Unchanged)
        .expect("the comparison runs");
        assert!(
            report.to_string().contains("no comparable baseline"),
            "{report}"
        );
        assert!(
            report
                .release_shortfall()
                .expect("a raised floor gates on its own")
                .to_string()
                .contains("0.59.0")
        );
    }

    /// A break at the frozen bootstrap fails both commands even when the
    /// candidate already clears the ordinary release-size gate. Both SemVer
    /// eras are stated because neither a pre-1.0 minor nor a post-1.0 major can
    /// buy a change to the attachment bootstrap.
    #[test]
    fn frozen_bootstrap_drift_is_unreleasable_at_any_sufficient_version() {
        let connect = json!({
            "delivery": "query",
            "family": "supervisor",
            "kind": "query",
            "path": "supervisor/connect",
            "payload": Value::Null,
            "record": "endpoint",
            "request": {"fields": [], "kind": "struct"},
            "response": {"fields": [], "kind": "struct"},
        });
        for (era, baseline, sufficient_candidate) in [
            ("pre-1.0", Version::new(0, 58, 1), Version::new(0, 59, 0)),
            ("post-1.0", Version::new(1, 4, 2), Version::new(2, 0, 0)),
        ] {
            let report = check(
                baseline,
                sufficient_candidate,
                FixtureSurfaces::new(std::slice::from_ref(&connect), &[]),
            )
            .run(CompatibilityImpact::Unchanged)
            .expect("the comparison runs");
            assert!(
                report.to_string().contains("[FROZEN BOOTSTRAP]"),
                "{era}: {report}"
            );
            assert!(
                report.release_shortfall().is_none(),
                "{era}: the candidate deliberately clears the ordinary breaking-change gate: \
                 {report}"
            );
            for (command, gates_the_release) in [("report", false), ("check-release", true)] {
                let failure = report
                    .command_failure(gates_the_release)
                    .unwrap_or_else(|| {
                        panic!("{era}: compatibility {command} must refuse frozen drift")
                    });
                assert!(
                    matches!(failure, CompatibilityFailure::FrozenBootstrapDrift),
                    "{era} compatibility {command}: {failure}"
                );
                assert!(
                    failure.to_string().contains("unreleasable"),
                    "{era} compatibility {command}: {failure}"
                );
            }
        }
    }
}
