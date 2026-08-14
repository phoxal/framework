//! The authored-source leg: what an older framework's reader made of the
//! documents a human wrote, compared against what this workspace's reader makes
//! of the same documents.
//!
//! Source compatibility is *directional*. A newer compatible framework keeps
//! reading every document an older compatible framework accepted, and keeps
//! reading it to mean the same thing. The inverse is not owed: an older reader
//! is allowed to reject a document written in a newer source language, because
//! nobody wrote that document before the language grew.
//!
//! `phoxal-manifest` is deliberately not a wire surface - no two binaries
//! negotiate over it - so nothing in the contract-surface leg can see a YAML
//! grammar break. This leg is what sees it: the same two-probe mechanism, over a
//! corpus of the authored documents the repository already keeps, comparing the
//! compiled canonical model rather than a schema.
//!
//! Nothing is stored for the comparison to read. The baseline is the published
//! crate, exactly as it is in the contract-surface leg, and the corpus is
//! ordinary in-repo test data that a fixture change edits in the open.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::probe::{Side, cargo_command, write_if_changed};
use crate::surface::{CompatibilityImpact, FieldDifference};

/// One authored project the corpus reads.
///
/// A project is the unit the compiler accepts: a robot manifest, the root its
/// relative paths resolve against, and the tree its component types are
/// resolved from. Every path is relative to the workspace root, so the corpus
/// is the same list in a checkout and in CI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AuthoredProject {
    /// The robot manifest, which is also what a diagnostic names this entry by.
    manifest: &'static str,
    /// The base for the manifest's own relative paths.
    project_root: &'static str,
    /// The tree holding one directory per authorable component type.
    components_root: &'static str,
}

/// Every authored project this repository keeps, in a stated order.
///
/// The list is deliberate rather than discovered: a corpus that globbed the
/// tree would quietly shrink when a document moved, and the gate would go green
/// by reading less. Between them these four mount every component type the
/// repository authors, in both the real-clock and the simulated grammar, with
/// and without simulation models, plus the example a reader of the framework
/// meets first.
const CORPUS: [AuthoredProject; 4] = [
    AuthoredProject {
        manifest: "fixture/robot/rgbd-imu-diff-drive/robot.yaml",
        project_root: "fixture/robot/rgbd-imu-diff-drive",
        components_root: "fixture/components",
    },
    AuthoredProject {
        manifest: "fixture/robot/rgbd-imu-diff-drive/robot.simulated.yaml",
        project_root: "fixture/robot/rgbd-imu-diff-drive",
        components_root: "fixture/components",
    },
    AuthoredProject {
        manifest: "fixture/robot/rgbd-imu-diff-drive/robot.simulated-no-models.yaml",
        project_root: "fixture/robot/rgbd-imu-diff-drive",
        components_root: "fixture/components",
    },
    AuthoredProject {
        manifest: "examples/hello-rover/robot.yaml",
        project_root: "examples/hello-rover",
        components_root: "examples/hello-rover/components",
    },
];

/// What one reader made of one authored project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Reading {
    /// The reader compiled the project, to exactly this canonical model.
    Accepted { canonical: Value },
    /// The reader refused the project, saying this.
    Rejected { error: String },
}

impl Reading {
    /// The reading a probe's document states.
    fn read(document: &Value) -> Result<Self> {
        let accepted = document
            .get("accepted")
            .and_then(Value::as_bool)
            .context("an authored-source probe printed no `accepted` verdict")?;
        if !accepted {
            return Ok(Self::Rejected {
                error: document
                    .get("error")
                    .and_then(Value::as_str)
                    .context("a rejecting authored-source probe printed no `error`")?
                    .to_owned(),
            });
        }
        Ok(Self::Accepted {
            canonical: document
                .get("canonical")
                .context("an accepting authored-source probe printed no `canonical` model")?
                .clone(),
        })
    }
}

/// Where the checker reads one side's reading of the corpus.
///
/// Compiling is the only faithful implementation; the drill and the tests state
/// readings outright so the directional rules are proved without one.
pub(crate) trait AuthoredDocuments {
    /// Read every corpus project through one crate set.
    fn read(&self, side: &Side) -> Result<BTreeMap<&'static str, Reading>>;
}

/// The corpus, read by compiling a probe against one crate set and running it
/// once per project.
pub(crate) struct SourceProbe {
    workspace_root: PathBuf,
}

impl SourceProbe {
    /// Probe the workspace this runner is part of.
    pub(crate) fn for_workspace() -> Result<Self> {
        Ok(Self {
            workspace_root: crate::workspace_root()?,
        })
    }

    /// The probe project for one side, materialized and ready to run.
    fn materialize(&self, side: &Side) -> Result<PathBuf> {
        let directory = self
            .workspace_root
            .join("target")
            .join("xtask-compat")
            .join(format!("source-{}", side.directory_name()));
        fs::create_dir_all(directory.join("src")).with_context(|| {
            format!(
                "failed to create the authored-source probe at {}",
                directory.display()
            )
        })?;
        write_if_changed(&directory.join("Cargo.toml"), &self.manifest(side))?;
        write_if_changed(&directory.join("src").join("main.rs"), PROGRAM)?;
        Ok(directory)
    }

    /// A refusal, with the checkout it was produced in taken back out of it.
    ///
    /// The reader names the document by absolute path, which is the right thing
    /// for somebody compiling their own project and noise in a report that
    /// already names the same document by its place in this tree.
    fn relative(&self, reading: Reading) -> Reading {
        let Reading::Rejected { error } = reading else {
            return reading;
        };
        Reading::Rejected {
            error: error.replace(&format!("{}/", self.workspace_root.display()), ""),
        }
    }

    /// The probe manifest: its own workspace, so the enclosing one neither
    /// adopts the generated package nor resolves its dependencies.
    ///
    /// Only `phoxal-manifest` is named. It pins its own `phoxal-model` and
    /// `phoxal-runtime-contract` exactly, so naming them again would state the
    /// same fact twice and let the two statements disagree.
    fn manifest(&self, side: &Side) -> String {
        const HEAD: &str = r#"# Written by `cargo xtask compatibility`. It is regenerated on every
# run, lives under `target/`, and is not tracked.
[workspace]

[package]
name = "phoxal-authored-source-probe"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
serde_json = "1"
"#;
        let reader = match side {
            Side::Baseline(train) => {
                format!("phoxal-manifest = \"={}\"\n", train.version)
            }
            Side::Current => format!(
                "phoxal-manifest = {{ path = {:?} }}\n",
                self.workspace_root.join("crates/manifest")
            ),
        };
        format!("{HEAD}{reader}")
    }
}

/// The probe program.
///
/// One source serves both sides: it names `SourceSet` and `compile`, the entry
/// the CLI itself compiles a project through, and nothing below them. That is
/// the point of the comparison - the two readers are asked the same question in
/// the same words, so a difference in the answer is a difference in the reader.
const PROGRAM: &str = r#"//! Written by `cargo xtask compatibility`. It compiles one authored
//! project through the public source entry and prints what this reader
//! made of it, as one JSON document.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let mut next = |what: &str| {
        PathBuf::from(
            arguments
                .next()
                .unwrap_or_else(|| panic!("the probe needs a {what}")),
        )
    };
    let project_root = next("project root");
    let robot_manifest = next("robot manifest");
    let components_root = next("components root");

    let sources = phoxal_manifest::SourceSet {
        project_root,
        robot_manifest,
        component_roots: component_roots(&components_root),
    };
    let document = match sources.compile() {
        Ok(compiled) => serde_json::json!({"accepted": true, "canonical": canonical(&compiled)}),
        Err(error) => serde_json::json!({"accepted": false, "error": error.to_string()}),
    };
    println!("{document}");
}

/// Every authorable component type in the tree, whether this project mounts it
/// or not: the compiler reads the roots it needs by name and ignores the rest,
/// so resolving them off the directory layout asks the reader nothing about
/// which source language the manifest is written in.
fn component_roots(components_root: &std::path::Path) -> BTreeMap<String, PathBuf> {
    let mut entries = std::fs::read_dir(components_root)
        .expect("the components root reads")
        .map(|entry| entry.expect("a components root entry reads").path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
        .into_iter()
        .filter(|root| root.join("component.yaml").is_file())
        .map(|root| {
            let name = root
                .file_name()
                .expect("a component directory has a name")
                .to_str()
                .expect("a component directory name is UTF-8")
                .to_owned();
            (name, root)
        })
        .collect()
}

/// Everything the compilation produced, in one deterministic document.
///
/// Asset bodies are bytes copied off disk, so they are summarized by identity
/// and length: what this comparison is about is which assets the compiler
/// decided the model references, not the contents of a mesh.
fn canonical(compiled: &phoxal_manifest::CompiledProject) -> serde_json::Value {
    serde_json::json!({
        "robot": compiled.robot(),
        "services": compiled
            .services()
            .iter()
            .map(|service| serde_json::json!({
                "config": service.config,
                "id": service.id.as_str(),
            }))
            .collect::<Vec<_>>(),
        "drivers": compiled
            .drivers()
            .iter()
            .map(|driver| serde_json::json!({
                "component_instance": driver.component_instance.as_str(),
                "config": driver.config,
                "implementation": driver.implementation.as_str(),
            }))
            .collect::<Vec<_>>(),
        "router": compiled.router().map(|router| router.asset.as_str()),
        "assets": compiled
            .assets()
            .iter()
            .map(|(id, bytes)| serde_json::json!({"bytes": bytes.len(), "id": id.as_str()}))
            .collect::<Vec<_>>(),
    })
}
"#;

impl AuthoredDocuments for SourceProbe {
    fn read(&self, side: &Side) -> Result<BTreeMap<&'static str, Reading>> {
        let directory = self.materialize(side)?;
        eprintln!(
            "reading the {side} authored-source corpus (compiling {})",
            directory.display()
        );
        let mut readings = BTreeMap::new();
        for project in CORPUS {
            let absolute = |relative: &str| self.workspace_root.join(relative);
            let output = Command::new(cargo_command())
                .arg("run")
                .arg("--quiet")
                .arg("--")
                .arg(absolute(project.project_root))
                .arg(absolute(project.manifest))
                .arg(absolute(project.components_root))
                .current_dir(&directory)
                .output()
                .with_context(|| {
                    format!(
                        "failed to run the authored-source probe at {}",
                        directory.display()
                    )
                })?;
            if !output.status.success() {
                bail!(
                    "the {side} authored-source probe failed on {} in {}:\n{}",
                    project.manifest,
                    directory.display(),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let printed = String::from_utf8(output.stdout).with_context(|| {
                format!(
                    "the {side} probe printed no UTF-8 document for {}",
                    project.manifest
                )
            })?;
            let document: Value = serde_json::from_str(printed.trim()).with_context(|| {
                format!(
                    "the {side} probe printed no reading for {}: {printed}",
                    project.manifest
                )
            })?;
            readings.insert(project.manifest, self.relative(Reading::read(&document)?));
        }
        Ok(readings)
    }
}

/// What happened to one authored project between the two readers.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ProjectOutcome {
    /// Both readers accept it and compile it to the same canonical model.
    Compatible,
    /// The published reader accepted it and this one refuses it.
    Regressed { error: String },
    /// Both readers accept it and disagree about what it means.
    Reinterpreted { differences: Vec<FieldDifference> },
    /// The published reader refused it and this one accepts it: the source
    /// language grew.
    Grown,
    /// Neither reader accepts it.
    Unreadable,
}

impl ProjectOutcome {
    /// What this outcome does to a project already written against the
    /// published reader.
    ///
    /// Only the two directional breaks are breaking. Growth is an addition of
    /// the same kind an added endpoint is: existing documents keep their
    /// meaning, and something can now be written that could not be before. A
    /// document neither reader accepts constrains nothing, because no release
    /// changes what happens to it.
    fn impact(&self) -> CompatibilityImpact {
        match self {
            Self::Regressed { .. } | Self::Reinterpreted { .. } => CompatibilityImpact::Breaking,
            Self::Grown => CompatibilityImpact::Additive,
            Self::Compatible | Self::Unreadable => CompatibilityImpact::Unchanged,
        }
    }

    /// Whether this outcome breaks a document somebody already wrote.
    fn breaks_an_authored_document(&self) -> bool {
        self.impact() == CompatibilityImpact::Breaking
    }

    /// This outcome's spelling in a report.
    fn token(&self) -> &'static str {
        match self {
            Self::Compatible => "compatible",
            Self::Regressed { .. } => "regressed",
            Self::Reinterpreted { .. } => "reinterpreted",
            Self::Grown => "grown",
            Self::Unreadable => "unreadable",
        }
    }

    /// The classification of one project, read directionally: the published
    /// reader is the promise, and this one either keeps it or does not.
    fn between(baseline: &Reading, current: &Reading) -> Self {
        match (baseline, current) {
            (Reading::Accepted { .. }, Reading::Rejected { error }) => Self::Regressed {
                error: error.clone(),
            },
            (Reading::Accepted { canonical: was }, Reading::Accepted { canonical: is }) => {
                if was == is {
                    Self::Compatible
                } else {
                    Self::Reinterpreted {
                        differences: FieldDifference::between(was, is),
                    }
                }
            }
            (Reading::Rejected { .. }, Reading::Accepted { .. }) => Self::Grown,
            (Reading::Rejected { .. }, Reading::Rejected { .. }) => Self::Unreadable,
        }
    }
}

/// One project, and what happened to it.
#[derive(Clone, Debug)]
struct ProjectResult {
    manifest: &'static str,
    outcome: ProjectOutcome,
}

impl fmt::Display for ProjectResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:<13} {}", self.outcome.token(), self.manifest)?;
        if self.outcome.breaks_an_authored_document() {
            write!(formatter, "  [SOURCE-BREAKING]")?;
        }
        match &self.outcome {
            ProjectOutcome::Regressed { error } => {
                for line in error.lines() {
                    write!(formatter, "\n                {line}")?;
                }
            }
            ProjectOutcome::Reinterpreted { differences } => {
                for difference in differences {
                    write!(formatter, "\n                {difference}")?;
                }
            }
            ProjectOutcome::Compatible | ProjectOutcome::Grown | ProjectOutcome::Unreadable => {}
        }
        Ok(())
    }
}

/// What one reading of the whole corpus, against one baseline, found.
#[derive(Clone, Debug)]
pub(crate) struct SourceComparison {
    results: Vec<ProjectResult>,
}

impl SourceComparison {
    /// Classify every corpus project, directionally.
    ///
    /// Both sides must have read the same corpus: a project one side never
    /// reported is a probe that stopped covering a document, not a document
    /// with no reading.
    pub(crate) fn between(
        baseline: &BTreeMap<&'static str, Reading>,
        current: &BTreeMap<&'static str, Reading>,
    ) -> Result<Self> {
        let mut results = Vec::new();
        for manifest in baseline.keys().chain(current.keys()) {
            if results
                .iter()
                .any(|result: &ProjectResult| result.manifest == *manifest)
            {
                continue;
            }
            let reading = |side: &str, readings: &BTreeMap<&'static str, Reading>| {
                readings
                    .get(manifest)
                    .cloned()
                    .with_context(|| format!("the {side} reader reported nothing for {manifest}"))
            };
            results.push(ProjectResult {
                manifest,
                outcome: ProjectOutcome::between(
                    &reading("published", baseline)?,
                    &reading("workspace", current)?,
                ),
            });
        }
        results.sort_by_key(|result| result.manifest);
        Ok(Self { results })
    }

    /// The greatest impact any one project carries.
    pub(crate) fn impact(&self) -> CompatibilityImpact {
        self.results
            .iter()
            .map(|result| result.outcome.impact())
            .max()
            .unwrap_or_default()
    }

    /// Whether every project came through unchanged, which is what lets the
    /// report say so in one line instead of listing the corpus.
    pub(crate) fn is_clean(&self) -> bool {
        self.results
            .iter()
            .all(|result| result.outcome == ProjectOutcome::Compatible)
    }
}

impl fmt::Display for SourceComparison {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_clean() {
            let read = self.results.len();
            let projects = if read == 1 { "project" } else { "projects" };
            return write!(formatter, "compatible ({read} authored {projects})");
        }
        for result in &self.results {
            write!(formatter, "\n  {result}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use semver::Version;
    use serde_json::json;

    use super::*;

    /// The workspace root a corpus path is resolved against.
    fn corpus_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the runner sits in the workspace")
            .to_path_buf()
    }

    fn accepted(canonical: Value) -> Reading {
        Reading::Accepted { canonical }
    }

    fn rejected() -> Reading {
        Reading::Rejected {
            error: "failed to compile robot document: unknown field `wheel_radius`".to_owned(),
        }
    }

    fn corpus(readings: [(&'static str, Reading); 2]) -> BTreeMap<&'static str, Reading> {
        readings.into_iter().collect()
    }

    fn compare(
        baseline: [(&'static str, Reading); 2],
        current: [(&'static str, Reading); 2],
    ) -> SourceComparison {
        SourceComparison::between(&corpus(baseline), &corpus(current))
            .expect("both sides read the same corpus")
    }

    /// Every corpus entry names documents that are actually in the tree. A
    /// corpus that pointed at a moved file would fail as a probe error rather
    /// than as the missing coverage it is.
    #[test]
    fn every_corpus_project_exists_in_the_workspace() {
        let root = corpus_root();
        for project in CORPUS {
            for relative in [project.manifest, project.project_root] {
                assert!(
                    root.join(relative).exists(),
                    "{relative} is not in the tree"
                );
            }
            let components = root.join(project.components_root);
            assert!(
                components.is_dir(),
                "{} is not a components tree",
                project.components_root
            );
        }
    }

    /// Both readers accepting the same document with the same meaning is the
    /// only outcome that asks nothing of a release.
    #[test]
    fn an_unchanged_reading_is_compatible() {
        let comparison = compare(
            [
                ("a.yaml", accepted(json!({"id": "r"}))),
                ("b.yaml", rejected()),
            ],
            [
                ("a.yaml", accepted(json!({"id": "r"}))),
                ("b.yaml", rejected()),
            ],
        );
        assert_eq!(comparison.impact(), CompatibilityImpact::Unchanged);
        assert!(!comparison.is_clean(), "an unreadable document is reported");
        assert!(comparison.to_string().contains("unreadable    b.yaml"));
    }

    /// A whole corpus that came through untouched says so in one line, because
    /// listing four unchanged documents on every pull request buries the one
    /// run where something moved.
    #[test]
    fn a_clean_corpus_is_reported_in_one_line() {
        let comparison = compare(
            [
                ("a.yaml", accepted(json!({"id": "r"}))),
                ("b.yaml", accepted(json!(1))),
            ],
            [
                ("a.yaml", accepted(json!({"id": "r"}))),
                ("b.yaml", accepted(json!(1))),
            ],
        );
        let rendered = comparison.to_string();
        assert_eq!(rendered, "compatible (2 authored projects)", "{rendered}");
        assert_eq!(comparison.impact(), CompatibilityImpact::Unchanged);
    }

    /// A document the published reader accepted and this one refuses is the
    /// break the whole leg exists to catch, and the refusal it prints is named.
    #[test]
    fn a_document_that_stopped_compiling_is_source_breaking() {
        let comparison = compare(
            [
                ("a.yaml", accepted(json!({"id": "r"}))),
                ("b.yaml", accepted(json!(1))),
            ],
            [("a.yaml", rejected()), ("b.yaml", accepted(json!(1)))],
        );
        assert_eq!(comparison.impact(), CompatibilityImpact::Breaking);
        let rendered = comparison.to_string();
        assert!(rendered.contains("regressed     a.yaml"), "{rendered}");
        assert!(rendered.contains("[SOURCE-BREAKING]"), "{rendered}");
        assert!(
            rendered.contains("unknown field `wheel_radius`"),
            "{rendered}"
        );
    }

    /// A document that still compiles but no longer means the same thing is the
    /// harder break, and it is named by the first place the two models disagree.
    #[test]
    fn a_document_that_changed_meaning_is_source_breaking() {
        let comparison = compare(
            [
                ("a.yaml", accepted(json!({"wheel_radius_m": 0.05}))),
                ("b.yaml", accepted(json!(1))),
            ],
            [
                ("a.yaml", accepted(json!({"wheel_radius_m": 0.5}))),
                ("b.yaml", accepted(json!(1))),
            ],
        );
        assert_eq!(comparison.impact(), CompatibilityImpact::Breaking);
        let rendered = comparison.to_string();
        assert!(rendered.contains("reinterpreted a.yaml"), "{rendered}");
        assert!(rendered.contains("[SOURCE-BREAKING]"), "{rendered}");
        assert!(
            rendered.contains("$.wheel_radius_m: 0.05 -> 0.5"),
            "{rendered}"
        );
    }

    /// The rule is directional: a document the published reader could not read
    /// and this one can is the source language growing, which breaks nobody.
    #[test]
    fn a_document_the_published_reader_refused_is_growth() {
        let comparison = compare(
            [("a.yaml", rejected()), ("b.yaml", accepted(json!(1)))],
            [
                ("a.yaml", accepted(json!({"id": "r"}))),
                ("b.yaml", accepted(json!(1))),
            ],
        );
        assert_eq!(comparison.impact(), CompatibilityImpact::Additive);
        let rendered = comparison.to_string();
        assert!(rendered.contains("grown         a.yaml"), "{rendered}");
        assert!(!rendered.contains("SOURCE-BREAKING"), "{rendered}");
    }

    /// A break outranks growth: one corpus can hold both, and the release is
    /// sized by the worst of them.
    #[test]
    fn a_break_outranks_growth_in_one_corpus() {
        let comparison = compare(
            [("a.yaml", rejected()), ("b.yaml", accepted(json!(1)))],
            [("a.yaml", accepted(json!(1))), ("b.yaml", rejected())],
        );
        assert_eq!(comparison.impact(), CompatibilityImpact::Breaking);
    }

    /// A side that reported nothing for a document is a probe that stopped
    /// covering it, which is an error rather than a classification.
    #[test]
    fn a_document_only_one_side_reported_is_an_error() {
        let failure = SourceComparison::between(
            &corpus([
                ("a.yaml", accepted(json!(1))),
                ("b.yaml", accepted(json!(1))),
            ]),
            &[("a.yaml", accepted(json!(1)))].into_iter().collect(),
        )
        .expect_err("a half-read corpus is not comparable")
        .to_string();
        assert!(failure.contains("b.yaml"), "{failure}");
    }

    /// The baseline side pins the exact published reader: a caret requirement
    /// would let Cargo resolve a newer one and compare the workspace against
    /// itself.
    #[test]
    fn the_baseline_probe_pins_the_published_reader() {
        let probe = SourceProbe {
            workspace_root: PathBuf::from("/workspace"),
        };
        let baseline = probe.manifest(&Side::baseline(Version::new(0, 59, 1)));
        assert!(
            baseline.contains("phoxal-manifest = \"=0.59.1\""),
            "{baseline}"
        );
        assert!(baseline.contains("[workspace]"), "{baseline}");
        let current = probe.manifest(&Side::Current);
        assert!(
            current.contains("phoxal-manifest = { path = \"/workspace/crates/manifest\" }"),
            "{current}"
        );
    }

    /// One program serves both sides, and it reaches the public compile entry
    /// and nothing below it - a probe that named an internal stage would be
    /// comparing something no author can write.
    #[test]
    fn the_probe_program_compiles_through_the_public_entry() {
        assert!(PROGRAM.contains("phoxal_manifest::SourceSet"), "{PROGRAM}");
        assert!(PROGRAM.contains(".compile()"), "{PROGRAM}");
        assert!(!PROGRAM.contains("normalize"), "{PROGRAM}");
    }
}
