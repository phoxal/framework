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
//! Each rule lives in the module that owns it: [`artifact`] owns authored
//! package grammar, [`framework_executable`] owns exact non-catalog framework
//! executables, [`registry`] joins those disjoint sets for publication policy,
//! [`dependencies`] owns what the workspace's crates may depend on,
//! [`feature_gate`] owns where a consumer profile may be named,
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

mod artifact;
mod comment_reference;
mod dependencies;
mod executable;
mod feature_gate;
mod framework_executable;
mod nested_package;
mod registry;
mod sdk_surface;
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
/// The list is deliberately explicit because a new public package is a
/// release-owner decision, not an accidental consequence of placing a library
/// under `crates/` or a service owner.
///
/// Tool-owned private implementation that exists only to implement the
/// `cargo-phoxal` binary must live as a Rust module of that binary, not as
/// a nested Cargo package underneath it. Adding `tools/cargo-phoxal/<name>`
/// here would re-introduce the prohibited shape; the no-nested-Cargo.toml
/// rule below guards against that pattern.
pub(crate) const LIBRARY_CRATE_DIRS: [&str; 15] = [
    "phoxal",
    "supervisor",
    "phoxal/macros",
    "phoxal/build-support",
    // The shared serialized artifact format. Owned by the framework but
    // published through the Phoxal registry so external consumers can
    // deserialize recorded bundles without depending on the project
    // compiler.
    "internal/artifact-format",
    "services/motion",
    "services/navigation",
    "services/kinematics",
    "services/world",
    "services/safety",
    // Components combine a published library with a private executable; the
    // library half is reusable (e.g. the simulator application consumes
    // component types directly), so the package is recognized here.
    "components/bno085",
    "components/ddsm115",
    "components/oak_d_lite",
    "components/vl53l1x",
    "components/zed_f9p",
];

/// Workspace-only package directories that are intentionally outside the
/// published registry graph.
///
/// The contract fixtures include one library-plus-binary owner fixture and two
/// binary-only consumers. The hardware driver fixture is also a library-plus-
/// binary package so its acceptance implementation cannot accidentally become
/// an official artifact or a release candidate. The scenario fixture is a
/// library-only owner that constructs planned scenarios and validates them
/// against non-rover quanta without depending on the project compiler, the
/// simulator, or any service package.
pub(crate) const INTERNAL_CRATE_DIRS: [&str; 5] = [
    "tests/fixtures/contracts/producer",
    "tests/fixtures/contracts/consumer",
    "tests/fixtures/ports/consumer",
    "tests/fixtures/hardware/driver",
    "tests/fixtures/scenarios",
];

/// The subset of [`INTERNAL_CRATE_DIRS`] that carries a library target and is
/// therefore checked by the library-directory completeness rule.
pub(crate) const INTERNAL_LIBRARY_CRATE_DIRS: [&str; 3] = [
    "tests/fixtures/contracts/producer",
    "tests/fixtures/hardware/driver",
    "tests/fixtures/scenarios",
];

/// The package a library crate directory must hold, or `None` for a directory
/// that names no library crate location.
///
/// Framework libraries are `phoxal-<suffix>` at `crates/<suffix>`. The
/// consolidated library-plus-binary service package is
/// `phoxal-service-<suffix>` at `services/<suffix>`. The historical
/// `services/<suffix>/contract/` shape is not a recognized production
/// layout — the artifact classifier rejects nested manifests and the
/// dependency policy no longer permits two service crates for one
/// service, so the helper does not map that path. The `phoxal/` facade
/// and the `supervisor/` host live directly at their package names.
///
/// This is the whole reason the directory can be shortened at all. `crates/`
/// already says `phoxal`, so repeating it in every child would be the
/// provider spelled twice on one path.
pub(crate) fn library_package_name(directory: &str) -> Option<String> {
    match directory {
        "tests/fixtures/contracts/producer" => return Some("phoxal-contract-owner-fixture".into()),
        "tests/fixtures/hardware/driver" => return Some("phoxal-hardware-driver-fixture".into()),
        // The shared serialized artifact format is owned by the framework
        // but lives one directory deeper than `crates/<name>/` so the
        // crate prefix rule does not match. The package name is the
        // kebab-case mirror of the directory suffix.
        "internal/artifact-format" => return Some("phoxal-artifact-format".into()),
        // Unit 6 relocated the proc-macro and code-generation helpers beneath
        // the facade (`phoxal/macros`, `phoxal/build-support`) and the native
        // MuJoCo adapter to a new top-level `simulation/` root. The package
        // names are the historical `phoxal-<suffix>` form, which the
        // `crates/<suffix>` branch would not derive from these paths.
        "phoxal/macros" => return Some("phoxal-macros".into()),
        "phoxal/build-support" => return Some("phoxal-build".into()),
        _ => {}
    }
    if directory == FACADE {
        return Some(FACADE.to_owned());
    }
    if directory == "supervisor" {
        return Some("phoxal-supervisor".to_owned());
    }
    if let Some(rest) = directory.strip_prefix("services/") {
        // The consolidated service package is exactly `services/<suffix>/`
        // with the package name `phoxal-service-<suffix>`. Nested paths
        // such as `services/<suffix>/contract/` are deliberately not
        // recognised here; the artifact classifier rejects them and the
        // dependency policy refuses the parallel crate, so this helper
        // would otherwise encode a conflicting rule.
        if !rest.is_empty() && !rest.contains('/') {
            return Some(format!("{FACADE}-service-{rest}"));
        }
        return None;
    }
    if let Some(component) = directory.strip_prefix("components/") {
        // A component driver owns its library and binary in one package
        // (`phoxal-component-<id>` at `components/<id>/`). The library is
        // part of the artifact package; the library completeness rule
        // counts it as a discovered workspace library target.
        if !component.is_empty() && !component.contains('/') {
            return Some(format!("{FACADE}-component-{component}"));
        }
        return None;
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

pub(crate) fn is_library_directory(directory: &str) -> bool {
    LIBRARY_CRATE_DIRS.contains(&directory) || INTERNAL_LIBRARY_CRATE_DIRS.contains(&directory)
}

pub(crate) fn is_internal_package_directory(directory: &str) -> bool {
    INTERNAL_CRATE_DIRS.contains(&directory)
}

/// The workspace one run of the gate judges.
///
/// Cargo is asked for the member metadata once and every rule reads that one
/// answer, so a run cannot judge two different pictures of the same tree.
pub(crate) struct Subject {
    root: PathBuf,
    /// `cargo metadata --no-deps`: this workspace's own packages.
    members: Metadata,
    /// The executable sets the workspace declares, or the grammar
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
#[derive(Debug)]
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
const RULES: [Rule; 13] = [
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
        name: "registry packages have independent release policy",
        check: framework_executable::registry_packages_have_independent_release_policy,
    },
    Rule {
        name: "public library dependency direction is exact",
        check: dependencies::public_library_dependency_direction_is_exact,
    },
    Rule {
        name: "canonical crates and the framework executable keep forbidden edges absent",
        check:
            dependencies::canonical_crates_and_the_framework_executable_keep_forbidden_edges_absent,
    },
    Rule {
        name: "retired framework libraries stay absent from the dependency graph",
        check: dependencies::retired_framework_libraries_stay_absent,
    },
    Rule {
        name: "zenoh dependency profiles keep transport compression disabled",
        check: dependencies::zenoh_dependency_profiles_keep_transport_compression_disabled,
    },
    Rule {
        name: "feature gates live only in the framework crate root",
        check: feature_gate::feature_gates_live_only_in_the_crate_root,
    },
    Rule {
        name: "unit-test modules have an explicit owner",
        check: test_module_ownership::unit_test_modules_have_an_explicit_owner,
    },
    Rule {
        name: "comments carry no issue or decision references",
        check: comment_reference::comments_carry_no_issue_or_decision_references,
    },
    Rule {
        name: "tools keep their private implementation as Rust modules",
        check: nested_package::no_nested_private_cargo_package_under_a_tool,
    },
    Rule {
        name: "the SDK keeps server-state out of its public surface",
        check: sdk_surface::the_sdk_keeps_server_state_out_of_its_public_surface,
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
/// workspace member carrying a reusable library target must be listed as either
/// published or internal, and every listed directory must still hold one.
/// Every listed library is an explicit owner rather than an accidental
/// consequence of placing a package under a broad directory.
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
        // After the runtime cleanup, services own their contract library and
        // executable in one package: `services/<id>/` is both an artifact
        // and a reusable library crate. The artifact-discovery rule still
        // runs separately and validates the lib+bin shape; the library
        // completeness rule must keep counting these directories so the
        // LIBRARY_CRATE_DIRS list reflects the post-consolidation layout.
        if library_package_name(directory).as_deref() != Some(package.name.as_str()) {
            violations.push(Violation::new(format!(
                "library crate {directory} does not hold the package its directory names; a \
                 library crate is `phoxal-<suffix>` at `crates/<suffix>`, or the facade `phoxal` \
                 at the root, and this one holds {}",
                package.name
            )));
        }
        let is_internal = INTERNAL_LIBRARY_CRATE_DIRS.contains(&directory);
        let publish_is_valid = if is_internal {
            package.publish.as_deref() == Some(&[])
        } else {
            package.publish.as_deref() == Some(&[executable::PHOXAL_PROVIDER.to_owned()])
        };
        if !publish_is_valid {
            violations.push(Violation::new(format!(
                "library crate {directory} has publish = {:?}; expected {}",
                package.publish,
                if is_internal {
                    "publish = false for internal library support"
                } else {
                    "publish = [\"phoxal\"] for a registry library"
                }
            )));
        }
        discovered.push(directory.to_owned());
    }

    for directory in &discovered {
        if !is_library_directory(directory) {
            violations.push(Violation::new(format!(
                "{directory} carries a library target but is listed as neither a published nor an \
                 internal library crate"
            )));
        }
    }
    for directory in LIBRARY_CRATE_DIRS
        .iter()
        .chain(INTERNAL_LIBRARY_CRATE_DIRS.iter())
    {
        // The installation owner is part of the target release shape, but its
        // package is introduced by the project-tooling change. Keep this
        // policy valid on the preceding commit while requiring the package as
        // soon as its source directory exists.
        if !subject.root.join(directory).is_dir() {
            continue;
        }
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
            library_package_name("supervisor").as_deref(),
            Some("phoxal-supervisor")
        );
        assert_eq!(
            library_package_name("phoxal/macros").as_deref(),
            Some("phoxal-macros")
        );
        // Nested production paths are deliberately not recognised: the
        // artifact classifier rejects them and the dependency policy
        // refuses a parallel service crate. Negative fixture.
        assert_eq!(library_package_name("services/motion/contract"), None);
        assert_eq!(
            library_package_name("services/motion/contract/nested"),
            None
        );
        // The consolidated service directory map agrees with the historical
        // contract/ shape.
        assert_eq!(
            library_package_name("services/motion").as_deref(),
            Some("phoxal-service-motion")
        );
        // A hyphenated suffix maps through unchanged; no such crate exists
        // today, and the rule has to hold for the one that might.
        assert_eq!(
            library_package_name("crates/wire-schema").as_deref(),
            Some("phoxal-wire-schema")
        );

        assert_eq!(library_package_name("crates"), None);
        assert_eq!(library_package_name("crates/"), None);
        assert_eq!(library_package_name("phoxal/macros/inner"), None);
        assert_eq!(library_package_name("phoxal-macros"), None);
        assert_eq!(library_package_name("contracts"), None);
        assert_eq!(library_package_name("services/motion/contract/inner"), None);
        assert_eq!(library_package_name("cratesfoo"), None);
        // Tool implementation that exists only to implement one tool must
        // be a Rust module of that tool, not a Cargo package nested under
        // it. The directories under `tools/cargo-phoxal/<name>/` therefore
        // carry no canonical package identity.
        assert_eq!(library_package_name("tools/cargo-phoxal/project"), None);
        assert_eq!(library_package_name("tools/cargo-phoxal/installation"), None);
        assert_eq!(library_package_name("tools/cargo-phoxal/project/inner"), None);
        assert_eq!(library_package_name("tools/cargo-phoxal"), None);
    }

    /// Every listed directory must satisfy the rule, so neither list can
    /// become a place to smuggle a crate past it.
    #[test]
    fn every_listed_library_crate_directory_obeys_the_rule() {
        for directory in LIBRARY_CRATE_DIRS
            .iter()
            .chain(INTERNAL_LIBRARY_CRATE_DIRS.iter())
        {
            assert!(
                library_package_name(directory).is_some(),
                "{directory} is listed as a library crate but names no package"
            );
        }
    }

    #[test]
    fn every_public_owner_has_a_stable_registry_package_identity() {
        for (directory, package) in [
            ("phoxal", "phoxal"),
            ("supervisor", "phoxal-supervisor"),
            ("phoxal/macros", "phoxal-macros"),
            ("phoxal/build-support", "phoxal-build"),
            ("services/motion", "phoxal-service-motion"),
            ("services/navigation", "phoxal-service-navigation"),
            ("services/kinematics", "phoxal-service-kinematics"),
            ("services/world", "phoxal-service-world"),
            ("services/safety", "phoxal-service-safety"),
        ] {
            assert_eq!(library_package_name(directory).as_deref(), Some(package));
            assert!(
                is_library_package(package),
                "{package} is not a public library"
            );
        }
        assert!(!is_library_package("phoxal-contract-owner-fixture"));
        // Tool implementation that exists only to implement one tool must
        // be a Rust module of that tool, not a Cargo package nested under
        // it. There is no canonical `phoxal-<name>` package for these
        // directories and the library name lookup returns None.
        assert_eq!(library_package_name("tools/cargo-phoxal/project"), None);
        assert_eq!(
            library_package_name("tools/cargo-phoxal/installation"),
            None
        );
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
