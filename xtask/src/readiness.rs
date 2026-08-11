//! The flip-day gate: everything that has to be true before this workspace
//! releases a 1.0.
//!
//! Flipping to a Stable major changes what every later release means, and it
//! happens once. So the conditions are a command rather than a checklist
//! somebody remembers: the Stable semantics are drilled, the toolchain floor is
//! compared against what is actually published, the frozen bootstrap is proved
//! by its own tests, and the workspace is searched for policy language that
//! only holds below 1.0. What no machine can decide is printed at the end,
//! named, rather than left implied by a green run.

use std::fmt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

use crate::index::{PublishedVersions, SparseIndex};
use crate::rehearsal::{GroupKind, Rehearsal, RehearsalReport};
use crate::toolchain::{ToolchainFloor, workspace_floor};

/// Language that is true only below 1.0.
///
/// The list is deliberately short and each entry is a phrase that cannot be
/// read any other way, because a broad pattern would collect false findings
/// and be turned off. A statement that names both eras is not on it: "pre-1.0
/// the line is the minor, from 1.0 on it is the major" stays true forever and
/// is exactly how these facts should be written.
const PRE_V1_ONLY_LANGUAGE: [ForbiddenPhrase; 5] = [
    ForbiddenPhrase {
        phrase: "exact framework-version equality",
        why: "two binaries interoperate when they share a compatibility line, not when their \
              versions are equal",
    },
    ForbiddenPhrase {
        phrase: "exact version equality",
        why: "the same claim, shorter: the comparison every validator makes is the line",
    },
    ForbiddenPhrase {
        phrase: "compatible when their versions are equal",
        why: "the same claim in prose",
    },
    ForbiddenPhrase {
        phrase: "a purely additive one included",
        why: "the pre-1.0 rule that every wire change carries the breaking marker. From 1.0 on an \
              addition is an ordinary minor and carries no marker, so this rule has to be \
              rewritten rather than inherited",
    },
    ForbiddenPhrase {
        phrase: "runs one framework train",
        why: "an execution runs one compatibility line; artifacts built from different trains on \
              that line share it",
    },
];

/// The file kinds the search reads.
///
/// Everything that states policy: source, documentation, manifests, workflows.
const SEARCHED_SUFFIXES: [&str; 6] = [".rs", ".md", ".toml", ".yml", ".yaml", ".sh"];

/// Files whose whole point is to carry the phrases, and which would otherwise
/// report themselves.
const SEARCH_EXEMPT: [&str; 1] = ["xtask/src/readiness.rs"];

/// One phrase the workspace may not carry after the flip.
struct ForbiddenPhrase {
    phrase: &'static str,
    why: &'static str,
}

/// One run of the flip-day gate.
pub(crate) struct V1Readiness {
    workspace_root: PathBuf,
    /// Whether a raised toolchain floor is a deliberate part of this flip.
    allow_rust_version_raise: bool,
}

impl V1Readiness {
    pub(crate) fn new(workspace_root: PathBuf, allow_rust_version_raise: bool) -> Self {
        Self {
            workspace_root,
            allow_rust_version_raise,
        }
    }

    /// Drill the Stable semantics, then check everything else the flip needs.
    pub(crate) fn run(&self) -> Result<ReadinessReport> {
        let rehearsal = Rehearsal::new(self.workspace_root.clone()).run()?;
        let checks = vec![
            self.rehearsal_check(&rehearsal),
            self.bootstrap_check(&rehearsal),
            self.toolchain_check()?,
            self.language_check()?,
        ];
        Ok(ReadinessReport { rehearsal, checks })
    }

    /// The Stable semantics themselves.
    fn rehearsal_check(&self, rehearsal: &RehearsalReport) -> ReadinessCheck {
        if rehearsal.held() {
            return ReadinessCheck::passed(
                "the Stable-line semantics drill",
                "every row of the matrix held",
            );
        }
        ReadinessCheck::failed(
            "the Stable-line semantics drill",
            "read the FAIL rows above. A Stable semantics failure is a bug in the release \
             arithmetic or in the version type, and no version may be flipped over it.",
        )
    }

    /// The frozen bootstrap, reported as its own finding rather than drilled a
    /// second time: the rehearsal has already run exactly these tests.
    fn bootstrap_check(&self, rehearsal: &RehearsalReport) -> ReadinessCheck {
        if rehearsal.group_held(GroupKind::FrozenBootstrap) {
            return ReadinessCheck::passed(
                "the frozen bootstrap pin tests exist and pass",
                "proved by the pinned tests in the drill above",
            );
        }
        ReadinessCheck::failed(
            "the frozen bootstrap pin tests exist and pass",
            "STOP. A frozen bootstrap fact moved, or the test that pins it is gone. There is no \
             autonomous remedy: escalate to the maintainer with the diff. See xtask/README.md \
             \"When a gate fails\", rule 3.",
        )
    }

    /// The toolchain floor, against what is actually published.
    fn toolchain_check(&self) -> Result<ReadinessCheck> {
        let name = "the workspace toolchain floor is the published train's";
        let train = SparseIndex::crates_io()
            .latest_train()
            .context("the published train could not be resolved, so no floor can be compared")?;
        let workspace = workspace_floor(&self.workspace_root)?;
        let floor = ToolchainFloor::between(train.rust_version, workspace);
        let stated = format!("published train {}: {floor}", train.version);
        if floor.required_step().is_none() {
            return Ok(ReadinessCheck::passed(name, stated));
        }
        if self.allow_rust_version_raise {
            return Ok(ReadinessCheck::passed(name, stated).needing(
                "the toolchain floor raise is acknowledged, so the flip release states it in its \
                 notes and is at least a minor over the published train",
            ));
        }
        Ok(ReadinessCheck::failed(
            name,
            format!(
                "{stated}. Either revert the raise, or acknowledge it with \
                 `--allow-rust-version-raise` and release at least a minor. See xtask/README.md \
                 \"When a gate fails\", rule 4."
            ),
        ))
    }

    /// Policy language that only holds below 1.0.
    fn language_check(&self) -> Result<ReadinessCheck> {
        let name = "no pre-1.0-only policy language remains";
        let mut found = Vec::new();
        for path in self.searched_files()? {
            let content = std::fs::read_to_string(self.workspace_root.join(&path))
                .with_context(|| format!("failed to read {path}"))?
                .to_lowercase();
            for forbidden in &PRE_V1_ONLY_LANGUAGE {
                if content.contains(forbidden.phrase) {
                    found.push(format!(
                        "{path}: \"{}\" - {}",
                        forbidden.phrase, forbidden.why
                    ));
                }
            }
        }
        if found.is_empty() {
            return Ok(ReadinessCheck::passed(
                name,
                format!(
                    "{} phrases searched across the tracked workspace",
                    PRE_V1_ONLY_LANGUAGE.len()
                ),
            ));
        }
        Ok(ReadinessCheck::failed(
            name,
            format!(
                "rewrite each of these to state both eras, or delete the claim:\n  {}",
                found.join("\n  ")
            ),
        ))
    }

    /// Every tracked text file, so the search reads what the repository ships
    /// and not what a build left behind.
    fn searched_files(&self) -> Result<Vec<String>> {
        let output = Command::new("git")
            .args(["ls-files", "-z"])
            .current_dir(&self.workspace_root)
            .output()
            .context("failed to list the tracked files")?;
        if !output.status.success() {
            bail!(
                "`git ls-files` failed in {}: {}",
                self.workspace_root.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .split('\0')
            .filter(|path| !path.is_empty())
            .filter(|path| {
                SEARCHED_SUFFIXES
                    .iter()
                    .any(|suffix| path.ends_with(suffix))
            })
            .filter(|path| !SEARCH_EXEMPT.contains(path))
            .map(str::to_owned)
            .collect())
    }
}

/// One condition of the flip, and what it found.
struct ReadinessCheck {
    name: &'static str,
    /// What was found, whichever way it fell.
    detail: String,
    held: bool,
    /// What a human still has to decide even though the machine is satisfied.
    needs_a_human: Option<&'static str>,
}

impl ReadinessCheck {
    fn passed(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            detail: detail.into(),
            held: true,
            needs_a_human: None,
        }
    }

    fn failed(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            detail: detail.into(),
            held: false,
            needs_a_human: None,
        }
    }

    fn needing(mut self, item: &'static str) -> Self {
        self.needs_a_human = Some(item);
        self
    }
}

impl fmt::Display for ReadinessCheck {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "  {}  {}",
            if self.held { "PASS" } else { "FAIL" },
            self.name
        )?;
        for line in self.detail.lines() {
            writeln!(formatter, "        {line}")?;
        }
        Ok(())
    }
}

/// Everything the flip-day gate found.
pub(crate) struct ReadinessReport {
    rehearsal: RehearsalReport,
    checks: Vec<ReadinessCheck>,
}

impl ReadinessReport {
    /// Whether every machine-decidable condition holds.
    fn ready(&self) -> bool {
        self.checks.iter().all(|check| check.held)
    }

    /// A condition that does not hold refuses the flip.
    pub(crate) fn exit_code(&self) -> ExitCode {
        if self.ready() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}

impl fmt::Display for ReadinessReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "{}\n", self.rehearsal)?;
        writeln!(formatter, "v1 readiness")?;
        for check in &self.checks {
            write!(formatter, "{check}")?;
        }

        writeln!(formatter, "\nstill needs a human")?;
        let mut items = self
            .checks
            .iter()
            .filter(|check| !check.held)
            .map(|check| format!("\"{}\" does not hold; see its FAIL above", check.name))
            .chain(
                self.checks
                    .iter()
                    .filter_map(|check| check.needs_a_human)
                    .map(str::to_owned),
            )
            .collect::<Vec<_>>();
        // The flip itself is never a machine decision, whatever the gates say.
        items.push(
            "the flip to 1.0.0 is a maintainer's act: this gate proves the conditions, it does \
             not choose the version or the moment"
                .to_owned(),
        );
        items.push(
            "the release that carries the flip states its own impact; a checker sees structure, \
             never a meaning that changed under an unchanged shape"
                .to_owned(),
        );
        for item in &items {
            writeln!(formatter, "  - {item}")?;
        }
        write!(
            formatter,
            "\n{}",
            if self.ready() {
                "every machine check holds; the conditions above are the ones left"
            } else {
                "NOT READY: a machine check does not hold, and no version may be flipped over it"
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the runner sits in the workspace")
            .to_path_buf()
    }

    /// The search reads the repository's tracked text and nothing a build left
    /// behind, which is what keeps it from reporting a probe under `target/`.
    #[test]
    fn the_search_reads_tracked_text_files_only() {
        let files = V1Readiness::new(workspace_root(), false)
            .searched_files()
            .expect("the tracked files list");
        assert!(files.contains(&"CONTRIBUTING.md".to_owned()), "{files:?}");
        assert!(
            files.contains(&"xtask/src/check.rs".to_owned()),
            "{files:?}"
        );
        assert!(
            !files.iter().any(|path| path.starts_with("target/")),
            "{files:?}"
        );
        assert!(
            !files.iter().any(|path| path.ends_with(".png")),
            "{files:?}"
        );
        assert!(
            !files.contains(&"xtask/src/readiness.rs".to_owned()),
            "the phrase list must not report itself"
        );
        // An exemption naming a file that is not there has quietly stopped
        // applying, which is how the gate would start reporting itself.
        for exempt in SEARCH_EXEMPT {
            assert!(
                workspace_root().join(exempt).is_file(),
                "{exempt} is exempted from the search but does not exist"
            );
        }
    }

    /// Every forbidden phrase is written in lower case, because the search
    /// lower-cases what it reads: a capital in the list would never match.
    #[test]
    fn every_forbidden_phrase_is_searchable() {
        for forbidden in &PRE_V1_ONLY_LANGUAGE {
            assert_eq!(
                forbidden.phrase,
                forbidden.phrase.to_lowercase(),
                "{} would never match",
                forbidden.phrase
            );
            assert!(
                !forbidden.why.is_empty(),
                "{} states no reason",
                forbidden.phrase
            );
        }
    }

    /// The language search runs over the real workspace and reports what it
    /// finds; whether it finds anything is the flip's business, not this
    /// test's.
    #[test]
    fn the_language_search_runs_over_this_workspace() {
        let check = V1Readiness::new(workspace_root(), false)
            .language_check()
            .expect("the search runs");
        assert!(!check.detail.is_empty());
    }

    /// A failed condition refuses the flip, and the summary says so in words as
    /// well as in the exit code.
    #[test]
    fn a_failed_condition_refuses_the_flip() {
        let report = ReadinessReport {
            rehearsal: RehearsalReport::drilled_nothing(),
            checks: vec![
                ReadinessCheck::passed("a held condition", "what it found"),
                ReadinessCheck::failed("a broken condition", "what to do about it"),
            ],
        };
        assert!(!report.ready());
        let rendered = report.to_string();
        assert!(rendered.contains("  PASS  a held condition"), "{rendered}");
        assert!(
            rendered.contains("  FAIL  a broken condition"),
            "{rendered}"
        );
        assert!(
            rendered.contains("        what to do about it"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- \"a broken condition\" does not hold"),
            "{rendered}"
        );
        assert!(rendered.contains("NOT READY"), "{rendered}");
    }

    /// Everything a machine can settle can be settled, and the summary still
    /// names what it cannot: the flip is a maintainer's act either way.
    #[test]
    fn a_ready_workspace_still_names_what_only_a_human_can_decide() {
        let report = ReadinessReport {
            rehearsal: RehearsalReport::drilled_nothing(),
            checks: vec![
                ReadinessCheck::passed("a held condition", "what it found")
                    .needing("an acknowledged raise"),
            ],
        };
        assert!(report.ready());
        let rendered = report.to_string();
        assert!(rendered.contains("- an acknowledged raise"), "{rendered}");
        assert!(
            rendered.contains("the flip to 1.0.0 is a maintainer's act"),
            "{rendered}"
        );
        assert!(!rendered.contains("NOT READY"), "{rendered}");
    }
}
