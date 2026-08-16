//! The Stable-line drill: every post-1.0 code path, run on demand.
//!
//! The framework is pre-1.0, so the release arithmetic, the compatibility
//! comparison, and the frozen bootstrap all run their Stable branches only
//! inside `cargo test`. A branch that is exercised only by a test suite is a
//! branch nobody has watched behave. This drill runs the whole Stable matrix as
//! one command, prints what it found, and fails loudly - so the first time the
//! major axis matters is not the day it is flipped.
//!
//! It reaches no registry. The baselines are stated outright, because the drill
//! proves what the semantics do at a Stable version and not which version is
//! published. The facts that need the real framework types are proved by
//! running the pinned tests that own them, as a subprocess: the checker depends
//! on no framework crate, and a drill that reimplemented `FrameworkVersion` or
//! the frozen documents would prove only that the copy agrees with itself.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use semver::Version;
use serde_json::{Value, json};

use crate::check::CompatibilityCheck;
use crate::index::{PublishedTrain, PublishedVersion, PublishedVersions};
use crate::probe::{ContractSurfaces, Extraction, Side};
use crate::source::{AuthoredDocuments, CorpusReading, Reading};
use crate::surface::{CONTRACT_CRATES, CompatibilityImpact};
use crate::toolchain::RustVersion;

/// The pinned tests that own the Stable semantics of `FrameworkVersion`.
///
/// Named one by one, and run with `--exact`, so a rename cannot leave the drill
/// quietly running nothing.
const VERSION_SEMANTICS: PinnedTests = PinnedTests {
    what: "FrameworkVersion Stable semantics",
    package: "phoxal-runtime-contract",
    tests: &[
        "version::tests::compatibility_is_the_line_and_the_line_is_the_break",
        "version::tests::the_compatibility_line_is_the_minor_before_v1_and_the_major_after",
        "version::tests::a_line_is_spelled_as_the_release_it_names",
    ],
};

/// The pinned tests that own the frozen attachment bootstrap.
///
/// These are the facts that must survive a major flip untouched: the connect
/// key's spelling, the composed wire key it becomes, both document shapes, the
/// canonical framework-version spelling inside them at a Stable train, the
/// MessagePack round trip, and the diagnostic a foreign tag produces.
const FROZEN_BOOTSTRAP: PinnedTests = PinnedTests {
    what: "frozen bootstrap invariants",
    package: "phoxal-protocol",
    tests: &[
        "api::supervisor::connect::tests::the_bootstrap_key_is_pinned_to_its_literal",
        "api::supervisor::connect::tests::the_composed_bootstrap_wire_key_is_pinned_to_its_literal",
        "api::supervisor::connect::tests::the_bootstrap_documents_are_pinned_to_their_literal_json",
        "api::supervisor::connect::tests::the_bootstrap_documents_are_pinned_to_their_declared_wire_shape",
        "api::supervisor::connect::tests::the_bootstrap_documents_round_trip_on_the_bus_codec",
        "api::supervisor::connect::tests::a_foreign_schema_tag_fails_by_naming_itself",
    ],
};

/// The pinned tests that own the transport the bootstrap stands on.
///
/// A frozen document is worth nothing if the path to it moves: these are the
/// facts an attaching client traverses before it can decode the reply at all -
/// discovery, the key grammar and the execution's text form inside it, the
/// query envelopes on both reply legs, the encoding string and its named-field
/// codec, and the Zenoh wire protocol version underneath everything.
const FROZEN_BOOTSTRAP_TRANSPORT: PinnedTests = PinnedTests {
    what: "frozen bootstrap transport facts",
    package: "phoxal-bus",
    tests: &[
        "abi::tests::the_bootstrap_encoding_and_codec_are_pinned_to_their_literals",
        "metadata::tests::the_bootstrap_reply_attachment_is_pinned_to_its_literal_fields",
        "query::tests::the_bootstrap_error_leg_is_pinned_to_its_literal_fields",
        "session::tests::the_bootstrap_discovery_session_is_pinned_to_a_scouting_free_client",
        "session::tests::the_bootstrap_execution_text_form_is_pinned_to_the_session_id_spelling",
        "session::tests::the_bootstrap_key_grammar_is_pinned_to_its_literal",
        "session::tests::the_zenoh_wire_protocol_version_is_pinned_to_its_literal",
    ],
};

/// One run of the Stable-line drill.
pub(crate) struct Rehearsal {
    workspace_root: PathBuf,
}

impl Rehearsal {
    pub(crate) fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    /// Drill every Stable-line fact and collect what each one did.
    pub(crate) fn run(&self) -> Result<RehearsalReport> {
        let mut groups = vec![Group {
            name: "release arithmetic at a Stable baseline",
            kind: GroupKind::ReleaseArithmetic,
            checks: stable_release_arithmetic()?,
        }];
        for (kind, pinned) in [
            (GroupKind::VersionSemantics, VERSION_SEMANTICS),
            (GroupKind::FrozenBootstrap, FROZEN_BOOTSTRAP),
            (GroupKind::FrozenBootstrap, FROZEN_BOOTSTRAP_TRANSPORT),
        ] {
            eprintln!("drilling {} ({})", pinned.what, pinned.package);
            groups.push(Group {
                name: pinned.what,
                kind,
                checks: vec![pinned.run(&self.workspace_root)?],
            });
        }
        Ok(RehearsalReport { groups })
    }
}

/// What one drilled fact did.
struct Check {
    name: String,
    failure: Option<String>,
}

impl Check {
    fn passed(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            failure: None,
        }
    }

    fn failed(name: impl Into<String>, failure: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            failure: Some(failure.into()),
        }
    }
}

impl fmt::Display for Check {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.failure {
            None => write!(formatter, "  PASS  {}", self.name),
            Some(failure) => {
                writeln!(formatter, "  FAIL  {}", self.name)?;
                for line in failure.lines() {
                    writeln!(formatter, "        {line}")?;
                }
                Ok(())
            }
        }
    }
}

/// Which part of the Stable semantics a group covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GroupKind {
    ReleaseArithmetic,
    VersionSemantics,
    FrozenBootstrap,
}

/// One part of the drill, and everything it found.
struct Group {
    name: &'static str,
    kind: GroupKind,
    checks: Vec<Check>,
}

impl Group {
    fn held(&self) -> bool {
        self.checks.iter().all(|check| check.failure.is_none())
    }
}

/// Everything the drill found, in the order it found it.
pub(crate) struct RehearsalReport {
    groups: Vec<Group>,
}

impl RehearsalReport {
    /// Whether every drilled fact held.
    pub(crate) fn held(&self) -> bool {
        self.groups.iter().all(Group::held)
    }

    /// Whether one part of the drill held, for a gate that reports it as a
    /// finding of its own rather than re-running it.
    pub(crate) fn group_held(&self, kind: GroupKind) -> bool {
        self.groups
            .iter()
            .filter(|group| group.kind == kind)
            .all(Group::held)
    }

    /// A report with nothing in it, for a test about what surrounds one.
    #[cfg(test)]
    pub(crate) fn drilled_nothing() -> Self {
        Self { groups: Vec::new() }
    }

    /// A failed drill fails the command.
    pub(crate) fn exit_code(&self) -> ExitCode {
        if self.held() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}

impl fmt::Display for RehearsalReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "v1 rehearsal: the Stable-line semantics, drilled against no registry"
        )?;
        let mut drilled = 0;
        let mut failed = 0;
        for group in &self.groups {
            writeln!(formatter, "\n{}", group.name)?;
            for check in &group.checks {
                drilled += 1;
                failed += usize::from(check.failure.is_some());
                writeln!(formatter, "{check}")?;
            }
        }
        write!(formatter, "\n{drilled} drilled, {} held", drilled - failed)?;
        if failed > 0 {
            write!(formatter, ", {failed} FAILED")?;
        }
        Ok(())
    }
}

/// A baseline the drill states outright.
///
/// `versions` fails rather than answering: the drill proves what the semantics
/// do at a Stable version, and reaching a registry to find out which version is
/// published would make an offline command depend on the network.
struct StatedTrain {
    version: Version,
    floor: Option<&'static str>,
}

impl PublishedVersions for StatedTrain {
    fn versions(&self, crate_name: &str) -> Result<Vec<PublishedVersion>> {
        bail!("the rehearsal states its baseline outright and reads no registry ({crate_name})")
    }

    fn latest_train(&self) -> Result<PublishedTrain> {
        Ok(PublishedTrain::stated(
            self.version.clone(),
            self.floor.map(RustVersion::parse).transpose()?,
        ))
    }
}

/// Surfaces the drill states outright, so no probe is compiled.
struct StatedSurfaces {
    baseline: Extraction,
    current: Extraction,
}

impl ContractSurfaces for StatedSurfaces {
    fn extract(&self, side: &Side) -> Result<Extraction> {
        Ok(match side {
            Side::Baseline(_) => self.baseline.clone(),
            Side::Current => self.current.clone(),
        })
    }
}

/// What the two readers made of one stated authored document.
///
/// The drill needs no corpus and compiles no probe: the directional rule is
/// about what one reader accepted and the next one does, and that is a pair of
/// readings whichever documents produced them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatedSource {
    /// Both readers accept the document, identically.
    Unchanged,
    /// The published reader accepted it and this one rejects it.
    Regressed,
    /// Both readers accept it and disagree about what it means.
    Reinterpreted,
    /// The published reader rejected it and this one accepts it.
    Grown,
    /// Neither reader accepts it.
    Unreadable,
}

impl StatedSource {
    /// The one document this state describes, as each side read it.
    fn readings(self, side: &Side) -> BTreeMap<&'static str, Reading> {
        let accepted = |canonical: Value| Reading::Accepted { canonical };
        let rejected = || Reading::Rejected {
            error: "failed to compile robot document".to_owned(),
        };
        let published = matches!(side, Side::Baseline(_));
        let reading = match (self, published) {
            (Self::Unchanged, _) => accepted(json!({"wheel_radius_m": 0.05})),
            (Self::Regressed, true) | (Self::Grown, false) => {
                accepted(json!({"wheel_radius_m": 0.05}))
            }
            (Self::Regressed, false) | (Self::Grown, true) | (Self::Unreadable, _) => rejected(),
            (Self::Reinterpreted, true) => accepted(json!({"wheel_radius_m": 0.05})),
            (Self::Reinterpreted, false) => accepted(json!({"wheel_radius_m": 0.5})),
        };
        [("robot.yaml", reading)].into_iter().collect()
    }
}

impl AuthoredDocuments for StatedSource {
    fn read(&self, side: &Side) -> Result<CorpusReading> {
        Ok(CorpusReading::Corpus(self.readings(side)))
    }
}

/// One row of the Stable matrix: a stated world, and what the checker must say
/// about it.
struct Row {
    name: &'static str,
    baseline: Version,
    baseline_floor: &'static str,
    workspace: Version,
    workspace_floor: &'static str,
    declared: CompatibilityImpact,
    published: Vec<Value>,
    current: Vec<Value>,
    /// What the two readers made of the authored corpus.
    source: StatedSource,
    /// Fragments the printed report must carry.
    report_states: &'static [&'static str],
    /// Fragments the refusal must carry, or an empty list when the candidate
    /// must be accepted.
    refusal_states: &'static [&'static str],
}

impl Row {
    /// Run one row through the real checker and say whether it behaved.
    fn drill(self) -> Result<Check> {
        let report = CompatibilityCheck::new(
            StatedTrain {
                version: self.baseline,
                floor: Some(self.baseline_floor),
            },
            StatedSurfaces {
                baseline: Extraction::Surfaces(documents(&self.published)),
                current: Extraction::Surfaces(documents(&self.current)),
            },
            self.source,
            self.workspace,
            RustVersion::parse(self.workspace_floor)?,
        )
        .run(self.declared)
        .context("the stated comparison runs")?;

        let mut missing = Vec::new();
        let rendered = report.to_string();
        for fragment in self.report_states {
            if !rendered.contains(fragment) {
                missing.push(format!("the report does not state `{fragment}`"));
            }
        }
        let refusal = report
            .release_shortfall()
            .map(|shortfall| shortfall.to_string());
        match (&refusal, self.refusal_states.is_empty()) {
            (Some(refusal), true) => missing.push(format!("the candidate was refused: {refusal}")),
            (None, false) => missing.push("the candidate was accepted".to_owned()),
            (Some(refusal), false) => {
                for fragment in self.refusal_states {
                    if !refusal.contains(fragment) {
                        missing.push(format!("the refusal does not state `{fragment}`"));
                    }
                }
            }
            (None, true) => {}
        }
        if missing.is_empty() {
            return Ok(Check::passed(self.name));
        }
        missing.push(rendered);
        if let Some(refusal) = refusal {
            missing.push(refusal);
        }
        Ok(Check::failed(self.name, missing.join("\n")))
    }
}

/// One endpoint record, as a contract crate renders one.
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

/// The frozen bootstrap endpoint, under the identity the checker names it by.
fn connect() -> Value {
    json!({
        "delivery": "query",
        "family": "supervisor",
        "kind": "query",
        "path": "supervisor/connect",
        "payload": Value::Null,
        "record": "endpoint",
        "request": {"fields": [], "kind": "struct"},
        "response": {"fields": [], "kind": "struct"},
    })
}

/// One side's surface set: every contract crate present, the records on the
/// crate that owns endpoints.
fn documents(records: &[Value]) -> BTreeMap<String, Value> {
    CONTRACT_CRATES
        .iter()
        .map(|contract_crate| {
            let declared = if contract_crate.carrier == "phoxal-protocol" {
                records.to_vec()
            } else {
                Vec::new()
            };
            (
                contract_crate.carrier.to_owned(),
                json!({ "records": declared }),
            )
        })
        .collect()
}

/// The Stable-line matrix.
///
/// Every row states a baseline at or above 1.0, because that is the whole point:
/// below it the major axis does not exist and a break lands on the minor. Two
/// versions on one Stable line (1.4.x and 1.9.x) must behave as one line, and a
/// break must reach across to the next major and nowhere short of it.
fn stable_release_arithmetic() -> Result<Vec<Check>> {
    let same = || vec![endpoint("robot/a")];
    let grown = || vec![endpoint("robot/a"), endpoint("robot/b")];
    let rows = vec![
        Row {
            name: "unchanged contracts over 1.4.2 release as a patch",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 4, 3),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: same(),
            current: same(),
            report_states: &[
                "impact:    unchanged",
                "at least a patch (1.4.3",
                "contracts: unchanged",
            ],
            refusal_states: &[],
        },
        Row {
            name: "re-releasing the published 1.4.2 is not a release",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 4, 2),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: same(),
            current: same(),
            report_states: &["impact:    unchanged"],
            refusal_states: &["1.4.3", "rule 7"],
        },
        Row {
            name: "an addition over 1.4.2 refuses a patch",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 4, 3),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: same(),
            current: grown(),
            report_states: &["impact:    additive", "at least a minor (1.5.0"],
            refusal_states: &["contract change is additive", "1.5.0", "rule 7"],
        },
        Row {
            name: "an addition over 1.4.2 accepts the next minor and stays inside the line",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 5, 0),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: same(),
            current: grown(),
            report_states: &["impact:    additive"],
            refusal_states: &[],
        },
        Row {
            name: "a break over 1.4.2 refuses 1.9.0, which is the same Stable line",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 9, 0),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: same(),
            current: vec![],
            report_states: &["impact:    breaking", "at least a major (2.0.0", "removed"],
            refusal_states: &["contract change is breaking", "2.0.0", "rules 1 and 2"],
        },
        Row {
            name: "a break over 1.9.3 accepts the next major, which is the next line",
            baseline: Version::new(1, 9, 3),
            baseline_floor: "1.88",
            workspace: Version::new(2, 0, 0),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: same(),
            current: vec![],
            report_states: &["impact:    breaking", "at least a major (2.0.0"],
            refusal_states: &[],
        },
        Row {
            name: "releasing further than a Stable break requires stays allowed",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(3, 0, 0),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: same(),
            current: vec![],
            report_states: &["impact:    breaking"],
            refusal_states: &[],
        },
        Row {
            name: "a declared break raises an unchanged Stable surface to a major",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 5, 0),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Breaking,
            source: StatedSource::Unchanged,
            published: same(),
            current: same(),
            report_states: &[
                "impact:    breaking (declared; the surfaces themselves are unchanged)",
                "at least a major (2.0.0",
            ],
            refusal_states: &["2.0.0"],
        },
        Row {
            name: "a declared addition cannot talk a Stable break down to a minor",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 5, 0),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Additive,
            source: StatedSource::Unchanged,
            published: same(),
            current: vec![],
            report_states: &["impact:    breaking"],
            refusal_states: &["2.0.0"],
        },
        Row {
            name: "a raised toolchain floor over 1.4.2 requires a minor of its own",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 4, 3),
            workspace_floor: "1.90",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: same(),
            current: same(),
            report_states: &[
                "toolchain: 1.90 (raised from the published 1.88",
                "at least a minor (1.5.0",
            ],
            refusal_states: &["rust-version floor raised 1.88 -> 1.90", "rule 4"],
        },
        Row {
            name: "an unchanged toolchain floor over 1.4.2 constrains nothing",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 4, 3),
            workspace_floor: "1.88.0",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: same(),
            current: same(),
            report_states: &["toolchain: 1.88.0 (unchanged from the published train)"],
            refusal_states: &[],
        },
        Row {
            name: "a break at the frozen bootstrap over 1.4.2 stops the release by name",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(2, 0, 0),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: vec![connect()],
            current: vec![],
            // The major is large enough for the break, and the freeze is
            // reported anyway: no version buys a frozen fact.
            report_states: &["[FROZEN BOOTSTRAP]", "impact:    breaking"],
            refusal_states: &[],
        },
        // The authored-source axis at a Stable baseline. It is directional, so
        // the four states are not symmetric: losing a document a published
        // reader accepted is a break of the line, and gaining one is not.
        Row {
            name: "an unchanged authored corpus over 1.4.2 constrains nothing",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 4, 3),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: same(),
            current: same(),
            report_states: &["source:    compatible (1 authored project)"],
            refusal_states: &[],
        },
        Row {
            name: "a document the published reader accepted and this one rejects refuses 1.9.0",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 9, 0),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Regressed,
            published: same(),
            current: same(),
            report_states: &[
                "impact:    breaking (the authored source; the surfaces themselves are unchanged)",
                "at least a major (2.0.0",
                "regressed",
                "[SOURCE-BREAKING]",
                "contracts: unchanged",
            ],
            refusal_states: &["rejected or means something else", "2.0.0", "rule 8"],
        },
        Row {
            name: "a document that compiles to a different model over 1.4.2 accepts the next major",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(2, 0, 0),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Reinterpreted,
            published: same(),
            current: same(),
            report_states: &[
                "impact:    breaking",
                "reinterpreted",
                "$.wheel_radius_m: 0.05 -> 0.5",
            ],
            refusal_states: &[],
        },
        Row {
            name: "a source language that grew over 1.4.2 refuses a patch and takes the minor",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 4, 3),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Grown,
            published: same(),
            current: same(),
            report_states: &["impact:    additive", "at least a minor (1.5.0", "grown"],
            refusal_states: &["authored source language is additive", "1.5.0", "rule 7"],
        },
        Row {
            name: "a document neither reader accepts over 1.4.2 constrains nothing",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 4, 3),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unreadable,
            published: same(),
            current: same(),
            report_states: &["impact:    unchanged", "unreadable"],
            refusal_states: &[],
        },
    ];
    rows.into_iter().map(Row::drill).collect()
}

/// Tests that own a fact this drill cannot state for itself.
struct PinnedTests {
    /// What the drill calls the fact, in its own output.
    what: &'static str,
    /// The package that owns the tests.
    package: &'static str,
    /// The tests, by their full path, run with `--exact`.
    tests: &'static [&'static str],
}

impl PinnedTests {
    /// Run exactly these tests and require exactly these tests to have run.
    ///
    /// The count is checked, not only the exit status: a renamed test matches
    /// no filter, and a run of nothing exits zero. Without the count the drill
    /// would go green by covering less and less.
    fn run(&self, workspace_root: &Path) -> Result<Check> {
        let expected = self.tests.len();
        let output = Command::new(cargo_command())
            .arg("test")
            .args(["--package", self.package])
            .arg("--lib")
            .arg("--")
            .arg("--exact")
            .args(self.tests)
            .current_dir(workspace_root)
            .output()
            .with_context(|| format!("failed to run {}'s pinned tests", self.package))?;
        let name = format!(
            "{expected} pinned tests in {} (cargo test --package {} --lib)",
            self.package, self.package
        );
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            return Ok(Check::failed(
                name,
                format!("the pinned tests failed:\n{stdout}\n{stderr}"),
            ));
        }
        if !stdout.contains(&format!("{expected} passed")) {
            return Ok(Check::failed(
                name,
                format!(
                    "expected {expected} tests to run; a filter that matches nothing exits zero, \
                     so a renamed or deleted test looks like a pass. Filters:\n  {}\n{stdout}",
                    self.tests.join("\n  ")
                ),
            ));
        }
        Ok(Check::passed(name))
    }
}

/// The Cargo that invoked this runner, so the pinned tests build with the same
/// toolchain the workspace is being drilled with.
fn cargo_command() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The matrix is the drill: if a row stops holding, the drill has to say so
    /// rather than the suite quietly covering for it.
    #[test]
    fn every_stable_row_holds() {
        let checks = stable_release_arithmetic().expect("the matrix runs");
        assert!(checks.len() >= 17, "the Stable matrix lost rows");
        for check in &checks {
            assert!(
                check.failure.is_none(),
                "{}: {}",
                check.name,
                check.failure.clone().unwrap_or_default()
            );
        }
    }

    /// A row that states the wrong expectation fails rather than passing
    /// vacuously, which is what makes a green matrix mean anything.
    #[test]
    fn a_row_that_does_not_behave_is_reported() {
        let check = Row {
            name: "a break over 1.4.2 is not a patch",
            baseline: Version::new(1, 4, 2),
            baseline_floor: "1.88",
            workspace: Version::new(1, 4, 3),
            workspace_floor: "1.88",
            declared: CompatibilityImpact::Unchanged,
            source: StatedSource::Unchanged,
            published: vec![endpoint("robot/a")],
            current: vec![],
            report_states: &["impact:    unchanged"],
            refusal_states: &[],
        }
        .drill()
        .expect("the row runs");
        let failure = check.failure.expect("the stated expectation does not hold");
        assert!(failure.contains("impact:    unchanged"), "{failure}");
        assert!(failure.contains("the candidate was refused"), "{failure}");
    }

    /// The drill states its baselines, so nothing in it can reach a registry.
    #[test]
    fn the_drill_reads_no_registry() {
        let failure = StatedTrain {
            version: Version::new(1, 4, 2),
            floor: Some("1.88"),
        }
        .versions("phoxal")
        .expect_err("the drill has no registry to read")
        .to_string();
        assert!(failure.contains("reads no registry"), "{failure}");
    }

    /// A group's outcome is readable on its own, which is how the readiness
    /// gate reports the bootstrap without running it twice.
    #[test]
    fn a_group_reports_its_own_outcome() {
        let report = RehearsalReport {
            groups: vec![
                Group {
                    name: "held",
                    kind: GroupKind::VersionSemantics,
                    checks: vec![Check::passed("a")],
                },
                Group {
                    name: "broke",
                    kind: GroupKind::FrozenBootstrap,
                    checks: vec![Check::failed("b", "why")],
                },
            ],
        };
        assert!(report.group_held(GroupKind::VersionSemantics));
        assert!(!report.group_held(GroupKind::FrozenBootstrap));
        assert!(!report.held());
        let rendered = report.to_string();
        assert!(rendered.contains("  PASS  a"), "{rendered}");
        assert!(rendered.contains("  FAIL  b"), "{rendered}");
        assert!(rendered.contains("        why"), "{rendered}");
        assert!(
            rendered.contains("2 drilled, 1 held, 1 FAILED"),
            "{rendered}"
        );
    }
}
