//! What an official participant may reach for in the one framework library.
//!
//! `phoxal` carries every supported consumer role, so a service, a driver, the
//! brain, or a worked example *can* type `phoxal::session` and turn a robot
//! participant into something that attaches to executions, owns a simulated
//! world, or runs the supervisor. Cargo unifies features and cannot stop it, and
//! this repository does not pretend otherwise: the boundary between roles is
//! policy over the code this project ships, and this is that policy.
//!
//! It says nothing about downstream crates. Anyone may enable any profile of a
//! published library; what the framework owes them is that its own reference
//! participants demonstrate the participant surface and nothing else, so the
//! examples a robot developer reads are the ones a robot developer can write.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use cargo_metadata::Package;

use super::tracked_source;
use super::{Subject, Violation};

/// The trees holding this repository's official participants.
///
/// Services, component drivers, and the library's own examples: everything
/// that is authored *on* the participant surface rather
/// than being part of the framework or one of its host processes.
const OFFICIAL_PARTICIPANT_ROOTS: [&str; 3] = ["services/", "components/", "phoxal/examples/"];

/// One host module, and the two ways a participant could name it.
struct HostModule {
    /// The canonical path, which is also how a fully qualified use or
    /// expression spells it.
    path: &'static str,
    /// The identifiers it appears as inside a braced `use phoxal::{…}` list,
    /// where the crate prefix is factored out.
    ///
    /// Matched on adjacent whole identifiers rather than as text, which is what
    /// keeps `bus::session_id` from reading as the session profile.
    segments: &'static [&'static str],
}

/// The host modules a participant may never name.
const HOST_MODULES: [HostModule; 3] = [
    HostModule {
        path: "phoxal::session",
        segments: &["session"],
    },
    HostModule {
        path: "phoxal::simulator",
        segments: &["simulator"],
    },
    HostModule {
        path: "phoxal::supervisor::host",
        segments: &["supervisor", "host"],
    },
];

/// The profiles an official participant may not enable.
///
/// `authoring` is here for the same reason as the other three: source
/// compilation stops at bundle compilation and never links into a process that
/// runs a robot. The framework executables are held to that one separately -
/// they are not participants, and the supervisor legitimately enables its own
/// host profile.
const HOST_FEATURES: [&str; 4] = ["session", "simulator", "supervisor", "authoring"];

/// Where a braced use list of the framework crate opens.
const BRACED_USE: &str = "use phoxal::{";

/// Official participants author on the participant surface only.
pub(super) fn official_participants_reach_for_no_host_profile(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for relative in tracked_source::files(&subject.root)? {
        let Some(path) = relative.to_str() else {
            continue;
        };
        if !path.ends_with(".rs") || !is_official_participant_path(path) {
            continue;
        }
        let source = fs::read_to_string(subject.root.join(&relative))
            .with_context(|| format!("failed to read {path}"))?;
        for module in imported_host_modules(&source) {
            violations.push(Violation::new(format!(
                "{path} names {module}; an official participant authors on the participant \
                 surface and reaches no host profile of the framework"
            )));
        }
    }

    for package in &subject.members.packages {
        if !is_official_participant_package(&subject.root, package) {
            continue;
        }
        for dependency in &package.dependencies {
            if dependency.name.as_str() != super::FACADE {
                continue;
            }
            for feature in &dependency.features {
                if HOST_FEATURES.contains(&feature.as_str()) {
                    violations.push(Violation::new(format!(
                        "{} enables {}/{feature} ({:?}); an official participant takes the \
                         participant profile and no host one",
                        package.name,
                        super::FACADE,
                        dependency.kind
                    )));
                }
            }
        }
    }
    Ok(violations)
}

/// Whether a workspace-relative path is inside an official participant tree.
fn is_official_participant_path(path: &str) -> bool {
    OFFICIAL_PARTICIPANT_ROOTS
        .iter()
        .any(|root| path.starts_with(root))
}

/// Whether a workspace member is an official participant, by where its manifest
/// sits.
fn is_official_participant_package(root: &Path, package: &Package) -> bool {
    package
        .manifest_path
        .as_std_path()
        .strip_prefix(root)
        .ok()
        .and_then(|relative| relative.to_str())
        .is_some_and(is_official_participant_path)
}

/// Every host module one source file names, in the order they are declared.
///
/// Two spellings reach the same module: the fully qualified path, and a braced
/// use list that factors the crate prefix out. Both are reported under the
/// canonical path, because that is the surface the rule is about. Comments and
/// string literals are read too - a rule about a boundary that may not be
/// crossed has no reason to let the spelling survive in prose beside the code
/// that would cross it.
fn imported_host_modules(source: &str) -> Vec<&'static str> {
    let lists = braced_use_lists(source)
        .into_iter()
        .map(|list| identifiers(list).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    HOST_MODULES
        .iter()
        .filter(|module| {
            source.contains(module.path)
                || lists.iter().any(|tokens| {
                    tokens
                        .windows(module.segments.len())
                        .any(|window| window == module.segments)
                })
        })
        .map(|module| module.path)
        .collect()
}

/// The text inside every `use phoxal::{…}` list in `source`.
fn braced_use_lists(source: &str) -> Vec<&str> {
    let mut lists = Vec::new();
    for (start, _) in source.match_indices(BRACED_USE) {
        let open = start + BRACED_USE.len();
        let mut depth = 1_usize;
        let mut end = open;
        for (offset, character) in source[open..].char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        lists.push(&source[open..end.max(open)]);
    }
    lists
}

/// Identifier-like tokens, so a compound name containing a module's spelling is
/// not read as that module.
fn identifiers(source: &str) -> impl Iterator<Item = &str> {
    source
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fully qualified spelling is the obvious one, and the braced list is
    /// the one a rule matching only paths would miss.
    #[test]
    fn both_spellings_of_a_host_import_are_found() {
        assert_eq!(
            imported_host_modules("use phoxal::session::Session;\n"),
            ["phoxal::session"]
        );
        assert_eq!(
            imported_host_modules("let world = phoxal::simulator::SimulatorSession::open()?;\n"),
            ["phoxal::simulator"]
        );
        assert_eq!(
            imported_host_modules("use phoxal::{prelude::*, session::Session};\n"),
            ["phoxal::session"]
        );
        assert_eq!(
            imported_host_modules("use phoxal::{api, supervisor::host::run};\n"),
            ["phoxal::supervisor::host"]
        );
        assert_eq!(
            imported_host_modules("use phoxal::{\n    api,\n    simulator::{Frame, World},\n};\n"),
            ["phoxal::simulator"]
        );
    }

    /// A participant that stays on its own surface reports nothing, including
    /// when it names the supervisor's *contracts* - those are a wire family, not
    /// the host seam.
    #[test]
    fn the_participant_surface_itself_is_not_a_host_import() {
        for clean in [
            "use phoxal::api;\nuse phoxal::prelude::*;\n",
            "use phoxal::{api, bus::StateView, model::Robot};\n",
            "use phoxal::supervisor::api::Snapshot;\n",
            "let id = phoxal::bus::session_id();\n",
            "use phoxal::{identity::ExecutionId, version::FrameworkVersion};\n",
        ] {
            assert!(
                imported_host_modules(clean).is_empty(),
                "{clean}: {:?}",
                imported_host_modules(clean)
            );
        }
    }

    /// The trees are the repository's own participants; the framework, its host
    /// processes and the runner are not participants and are not judged here.
    #[test]
    fn only_the_official_participant_trees_are_judged() {
        assert!(is_official_participant_path("services/drive/src/drive.rs"));
        assert!(is_official_participant_path(
            "components/bno085/src/main.rs"
        ));
        assert!(is_official_participant_path("phoxal/examples/minimal.rs"));

        assert!(!is_official_participant_path("phoxal/src/session/mod.rs"));
        assert!(!is_official_participant_path("supervisor/src/main.rs"));
        assert!(!is_official_participant_path("xtask/src/policy/mod.rs"));
        assert!(!is_official_participant_path("crates/fixture/src/lib.rs"));
    }
}
