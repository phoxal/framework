//! `cargo xtask policy`: the rules the framework workspace must obey as a
//! whole.
//!
//! No single crate owns these facts - that a package's directory, name and
//! `publish` field agree, that the zenoh dependency set keeps transport
//! compression disabled, that no committed comment carries an issue or
//! decision reference - so they are a CI gate rather than a crate's tests.
//! Every rule reads `cargo metadata`, the filesystem or Git and nothing else,
//! which is what keeps this verb inside the runner's no-framework-crate rule:
//! the gate never builds the stack it judges, so it runs on a bare runner.
//!
//! The proofs that *do* need the framework linked - what a linked participant
//! binary carries, what the facade and the process-contract crate agree on,
//! what wire version the transport actually speaks - are ordinary tests in the
//! crates that own them, and run under `cargo test --workspace`.
//!
//! Each rule lives in the module that owns it: [`api_tree`] owns how the three
//! wire families are declared, [`artifact`] owns authored
//! package grammar, [`framework_executable`] owns exact non-catalog framework
//! executables, [`registry`] joins those disjoint sets for publication policy,
//! [`dependencies`] owns what the workspace's crates may depend on,
//! [`retired_surface`] owns what deleted surfaces may not bring back,
//! [`test_module_ownership`] owns where a unit-test module may sit,
//! [`comment_reference`] owns what a comment may name, and [`tracked_source`]
//! owns what counts as committed source for repository-wide scans. This root
//! owns only the workspace facts they share, and the report they are run into.

use std::cell::OnceCell;
use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use cargo_metadata::{Metadata, MetadataCommand};

mod api_tree;
mod artifact;
mod comment_reference;
mod dependencies;
mod executable;
mod framework_executable;
mod registry;
mod retired_surface;
mod test_module_ownership;
mod tracked_source;

/// The directory holding every library crate that carries a name suffix.
pub(crate) const LIBRARY_CRATE_ROOT: &str = "crates";

/// The facade crate, which is both a directory at the workspace root and the
/// package name every other library crate prefixes itself with.
pub(crate) const FACADE: &str = "phoxal";

/// The directories holding the workspace's published library crates, one
/// crate each, as paths relative to the workspace root.
///
/// These are outside the artifact grammar: they are libraries, not official
/// artifact packages, so discovery must skip them rather than reject them.
/// [`the_library_crate_list_matches_the_workspace_members`] fails when a
/// library crate is added to the workspace without being added here, because a
/// missing entry would silently turn that crate into a grammar violation.
pub(crate) const LIBRARY_CRATE_DIRS: [&str; 2] = ["phoxal", "crates/macros"];

/// Library crates that serve this workspace's own tests and reach no
/// registry. They carry a library target, so the completeness check below
/// still demands they be listed; they are simply listed here rather than in
/// [`LIBRARY_CRATE_DIRS`], which keeps them out of the published dependency
/// graph [`is_library_package`] describes.
///
/// `crates/fixture` obeys the library naming rule exactly - `phoxal-fixture`
/// at `crates/fixture` - and appears here only because it is unpublished, not
/// because it sits anywhere unusual.
pub(crate) const INTERNAL_CRATE_DIRS: [&str; 1] = ["crates/fixture"];

/// The package a library crate directory must hold, or `None` for a directory
/// that names no library crate location.
///
/// One rule, no exceptions: a library crate is `phoxal-<suffix>` and lives at
/// `crates/<suffix>`. The facade is the single crate whose name carries no
/// suffix, so it is the single crate that does not live in the suffix
/// directory; it sits at the workspace root as `phoxal/`. That falls out of
/// the rule rather than carving a hole in it.
///
/// This is the whole reason the directory can be shortened at all. `crates/`
/// already says `phoxal`, so repeating it in every child would be the
/// provider spelled twice on one path.
pub(crate) fn library_package_name(directory: &str) -> Option<String> {
    if directory == FACADE {
        return Some(FACADE.to_owned());
    }
    let suffix = directory
        .strip_prefix(LIBRARY_CRATE_ROOT)?
        .strip_prefix('/')?;
    // A library crate is one directory deep and no deeper: `crates/protocol/inner`
    // would be a second package hiding under the first one's name.
    if suffix.is_empty() || suffix.contains('/') {
        return None;
    }
    Some(format!("{FACADE}-{suffix}"))
}

/// Whether a package name is one of the workspace's published library crates.
///
/// Derived from [`LIBRARY_CRATE_DIRS`] through the same rule rather than
/// listed a second time, so the directories stay the single place a library
/// crate is declared. Deliberately excludes [`INTERNAL_CRATE_DIRS`]: an
/// unpublished crate has no place in the published dependency graph.
pub(crate) fn is_library_package(package_name: &str) -> bool {
    LIBRARY_CRATE_DIRS
        .iter()
        .any(|directory| library_package_name(directory).as_deref() == Some(package_name))
}

/// The workspace one run of the gate judges.
///
/// Cargo is asked for the member metadata once and every rule reads that one
/// answer, so a run cannot judge two different pictures of the same tree.
pub(crate) struct Subject {
    root: PathBuf,
    /// `cargo metadata --no-deps`: this workspace's own packages.
    members: Metadata,
    /// The two executable sets the workspace declares, or the grammar
    /// violation that stopped discovery. Several rules stand on it, so it is
    /// resolved at most once.
    executables: OnceCell<Result<registry::Workspace, String>>,
}

impl Subject {
    fn for_workspace() -> Result<Self> {
        let root = crate::workspace_root()?;
        let members = MetadataCommand::new()
            .manifest_path(root.join("Cargo.toml"))
            .no_deps()
            .exec()
            .context("failed to read workspace metadata")?;
        Ok(Self {
            root,
            members,
            executables: OnceCell::new(),
        })
    }

    /// The executable sets the workspace declares.
    ///
    /// Discovery validates the package grammar as it goes, so it can fail; a
    /// failure is exactly what the rules standing on it are looking for, and is
    /// returned as a finding rather than as an error that would abort the run
    /// before the other rules had spoken.
    fn executables(&self) -> Result<&registry::Workspace, Violation> {
        match self.executables.get_or_init(|| {
            registry::Workspace::from_metadata(&self.members).map_err(|error| format!("{error:#}"))
        }) {
            Ok(workspace) => Ok(workspace),
            Err(error) => Err(Violation::new(error.clone())),
        }
    }
}

/// One offending package, path or edge, stated the way the rule found it.
pub(crate) struct Violation(String);

impl Violation {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(&self.0)
    }
}

/// One rule, and the check that decides it.
struct Rule {
    name: &'static str,
    check: fn(&Subject) -> Result<Vec<Violation>>,
}

/// Every rule this gate enforces, in the order the report prints them:
/// workspace shape first, then what the crates may depend on, then what the
/// committed source may say.
const RULES: [Rule; 14] = [
    Rule {
        name: "the library crate list matches the workspace members",
        check: the_library_crate_list_matches_the_workspace_members,
    },
    Rule {
        name: "the official artifact release scope is exact",
        check: artifact::the_official_artifact_release_scope_is_exact,
    },
    Rule {
        name: "the supervisor is a default member and a non-catalog registry executable",
        check: framework_executable::the_supervisor_is_a_default_member_and_non_catalog_executable,
    },
    Rule {
        name: "every registry executable participates in the release train",
        check: framework_executable::every_registry_executable_participates_in_the_release_train,
    },
    Rule {
        name: "public library dependency direction is exact",
        check: dependencies::public_library_dependency_direction_is_exact,
    },
    Rule {
        name: "canonical crates and official participants keep forbidden edges absent",
        check: dependencies::canonical_crates_and_official_participants_keep_forbidden_edges_absent,
    },
    Rule {
        name: "zenoh dependency profiles keep transport compression disabled",
        check: dependencies::zenoh_dependency_profiles_keep_transport_compression_disabled,
    },
    Rule {
        name: "retired surfaces stay absent",
        check: retired_surface::retired_surfaces_stay_absent,
    },
    Rule {
        name: "participant kind declarations match the two explicit owners",
        check: retired_surface::participant_kind_declarations_match_the_two_explicit_owners,
    },
    Rule {
        name: "source-reference alias shims stay absent",
        check: retired_surface::source_reference_alias_shims_stay_absent,
    },
    Rule {
        name: "endpoint bindings come only from the declaration beside their payload",
        check: api_tree::endpoint_bindings_come_only_from_the_declaration,
    },
    Rule {
        name: "family-rooted keys are rendered by the api tree, never authored",
        check: api_tree::family_rooted_keys_are_rendered_not_authored,
    },
    Rule {
        name: "unit-test modules have an explicit owner",
        check: test_module_ownership::unit_test_modules_have_an_explicit_owner,
    },
    Rule {
        name: "comments carry no issue or decision references",
        check: comment_reference::comments_carry_no_issue_or_decision_references,
    },
];

/// Run every rule over this workspace and report what they found.
pub(crate) fn run() -> Result<PolicyReport> {
    let subject = Subject::for_workspace()?;
    let findings = RULES
        .iter()
        .map(|rule| {
            Ok(Finding {
                name: rule.name,
                violations: (rule.check)(&subject)
                    .with_context(|| format!("rule \"{}\" could not be decided", rule.name))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PolicyReport { findings })
}

/// What one rule found.
struct Finding {
    name: &'static str,
    violations: Vec<Violation>,
}

impl fmt::Display for Finding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "  {}  {}",
            if self.violations.is_empty() {
                "PASS"
            } else {
                "FAIL"
            },
            self.name
        )?;
        for violation in &self.violations {
            for line in violation.0.lines() {
                writeln!(formatter, "        {line}")?;
            }
        }
        Ok(())
    }
}

/// Everything the gate found, over every rule.
pub(crate) struct PolicyReport {
    findings: Vec<Finding>,
}

impl PolicyReport {
    fn failed(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| !finding.violations.is_empty())
            .count()
    }

    /// A rule that does not hold fails the gate.
    pub(crate) fn exit_code(&self) -> ExitCode {
        if self.failed() == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}

impl fmt::Display for PolicyReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "workspace policy")?;
        for finding in &self.findings {
            write!(formatter, "{finding}")?;
        }
        let failed = self.failed();
        if failed == 0 {
            write!(
                formatter,
                "\n{} rules checked, every rule holds",
                self.findings.len()
            )
        } else {
            write!(
                formatter,
                "\n{} rules checked, {failed} failed",
                self.findings.len()
            )
        }
    }
}

/// A hand-maintained list that silently skips validation when it goes stale is
/// worse than no list, so the workspace itself is the authority: every
/// workspace member carrying a library target must be listed as either
/// published or internal, and every listed directory must still hold one. Both
/// lists obey the naming rule, so being unpublished buys a crate no leniency
/// about where it lives.
fn the_library_crate_list_matches_the_workspace_members(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    let mut discovered = Vec::new();
    for package in subject.members.workspace_packages() {
        if !package
            .targets
            .iter()
            .any(|target| target.kind.iter().any(executable::is_library_target_kind))
        {
            continue;
        }
        let crate_dir = package
            .manifest_path
            .parent()
            .with_context(|| format!("{} manifest has no parent", package.name))?
            .as_std_path();
        let relative = crate_dir
            .strip_prefix(&subject.root)
            .with_context(|| format!("{} is not under the workspace root", crate_dir.display()))?;
        let directory = relative
            .to_str()
            .with_context(|| format!("{} is not a UTF-8 workspace path", relative.display()))?;
        if library_package_name(directory).as_deref() != Some(package.name.as_str()) {
            violations.push(Violation::new(format!(
                "library crate {directory} does not hold the package its directory names; a \
                 library crate is `phoxal-<suffix>` at `crates/<suffix>`, or the facade `phoxal` \
                 at the root, and this one holds {}",
                package.name
            )));
        }
        discovered.push(directory.to_owned());
    }

    for directory in &discovered {
        if !LIBRARY_CRATE_DIRS.contains(&directory.as_str())
            && !INTERNAL_CRATE_DIRS.contains(&directory.as_str())
        {
            violations.push(Violation::new(format!(
                "{directory} carries a library target but is listed as neither a published nor an \
                 internal library crate"
            )));
        }
    }
    for directory in LIBRARY_CRATE_DIRS.iter().chain(INTERNAL_CRATE_DIRS.iter()) {
        if !discovered.iter().any(|found| found == directory) {
            violations.push(Violation::new(format!(
                "{directory} is listed as a library crate but no workspace member with a library \
                 target lives there"
            )));
        }
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule is stated once as a function, so the cases that must *not*
    /// resolve are as much a part of it as the cases that must. A directory
    /// deeper than one level is the interesting one: it would otherwise let a
    /// second package hide under the first one's name.
    #[test]
    fn a_library_crate_directory_names_exactly_one_package() {
        assert_eq!(library_package_name("phoxal").as_deref(), Some("phoxal"));
        assert_eq!(
            library_package_name("crates/macros").as_deref(),
            Some("phoxal-macros")
        );
        // Being unpublished changes nothing about where a crate lives.
        assert_eq!(
            library_package_name("crates/fixture").as_deref(),
            Some("phoxal-fixture")
        );
        // A hyphenated suffix maps through unchanged; no such crate exists
        // today, and the rule has to hold for the one that might.
        assert_eq!(
            library_package_name("crates/wire-schema").as_deref(),
            Some("phoxal-wire-schema")
        );

        assert_eq!(library_package_name("crates"), None);
        assert_eq!(library_package_name("crates/"), None);
        assert_eq!(library_package_name("crates/macros/inner"), None);
        assert_eq!(library_package_name("phoxal-macros"), None);
        assert_eq!(library_package_name("services/drive"), None);
        assert_eq!(library_package_name("cratesfoo"), None);
    }

    /// Every listed directory must satisfy the rule, so neither list can
    /// become a place to smuggle a crate past it.
    #[test]
    fn every_listed_library_crate_directory_obeys_the_rule() {
        for directory in LIBRARY_CRATE_DIRS.iter().chain(INTERNAL_CRATE_DIRS.iter()) {
            assert!(
                library_package_name(directory).is_some(),
                "{directory} is listed as a library crate but names no package"
            );
        }
    }

    /// A rule that holds prints as a single PASS line; one that does not names
    /// every violation under its own FAIL, and the gate refuses the tree.
    #[test]
    fn the_report_names_every_violation_under_its_rule() {
        let report = PolicyReport {
            findings: vec![
                Finding {
                    name: "a rule that holds",
                    violations: Vec::new(),
                },
                Finding {
                    name: "a rule that does not",
                    violations: vec![Violation::new("phoxal-macros -> phoxal")],
                },
            ],
        };
        let rendered = report.to_string();
        assert!(rendered.contains("  PASS  a rule that holds"), "{rendered}");
        assert!(
            rendered.contains("  FAIL  a rule that does not"),
            "{rendered}"
        );
        assert!(
            rendered.contains("        phoxal-macros -> phoxal"),
            "{rendered}"
        );
        assert!(rendered.contains("2 rules checked, 1 failed"), "{rendered}");
    }
}
