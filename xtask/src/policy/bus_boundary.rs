//! What only the bus module may do: speak to the transport, and say what an
//! endpoint's key is.
//!
//! Both rules exist because the same thing goes wrong when they are broken. A
//! crate that reaches Zenoh directly holds a second, unbranded copy of the
//! transport, which the typed handle vocabulary can no longer make any promise
//! about. A module that writes out `"robot/drive/state"` holds a second copy of
//! the topic tree, which drifts from the declaration the moment a module is
//! renamed - and a renamed semantic module *is* a wire change, which is exactly
//! what the compatibility gate is supposed to be able to see.
//!
//! Neither rule judges the runner or the fixture crate. `xtask` links no
//! framework crate, publishes on no bus, and its topic-shaped literals are
//! stated fixtures for the checker's own matrix; `crates/fixture` is
//! unpublished dev-only test support that no robot ever links. A rule that read
//! either would be reporting its own test data.

use std::fs;

use anyhow::{Context, Result};

use super::tracked_source;
use super::{Subject, Violation};

/// The module that owns the transport, and the only place raw Zenoh may be
/// named.
const BUS_MODULE: &str = "phoxal/src/bus/";

/// The file that owns the endpoint tree: the private `nodes!` and `endpoints!`
/// declarations every topic and every endpoint implementation derives from.
const ENDPOINT_TREE: &str = "phoxal/src/bus/tree.rs";

/// The workspace trees this pair of rules does not read.
///
/// `xtask/` is the runner: it links no framework crate and publishes on no bus.
/// `crates/fixture/` is unpublished dev-only test support that no robot links.
const UNJUDGED_TREES: [&str; 2] = ["xtask/", "crates/fixture/"];

/// How raw transport access is spelled.
const TRANSPORT_TOKENS: [&str; 2] = ["zenoh::", "use zenoh"];

/// The family roots every framework topic starts with.
///
/// Matched as the opening of a string literal, because a complete topic string
/// is exactly what nothing outside the tree may author.
const TOPIC_ROOTS: [&str; 3] = ["\"robot/", "\"runtime/", "\"supervisor/"];

/// The one literal beginning with a family root that is not a topic.
///
/// A compiled bundle's asset tree is rooted at `robot/` too - `robot/meshes/…`
/// is the path below `<bundle>/assets` that the manifest's structure resolves
/// against - so the asset root and the robot *family* root collide as text and
/// nothing but their meaning tells them apart. The rule deliberately reads past
/// this one: an asset id is a filesystem layout the bundle owns, not a key any
/// peer subscribes to, and the topic tree has no say in it.
const ASSET_TREE_ROOT: &str = "\"robot/meshes";

/// The one key that is frozen as a literal on purpose: the supervisor's presence
/// lease beside the frozen attachment bootstrap. It is not an endpoint, so no
/// declaration renders it, and it must never move, so it is pinned by the tests
/// that own it rather than derived. Read past here, and nowhere else.
const PRESENCE_LEASE: (&str, &str) = (
    "phoxal/src/supervisor/api/connect.rs",
    "\"supervisor/presence\"",
);

/// The traits an endpoint declaration implements.
const ENDPOINT_TRAITS: [&str; 2] = ["QueryEndpoint", "Endpoint"];

/// Paths an `impl` may qualify those traits with.
const ENDPOINT_TRAIT_PATHS: [&str; 2] = ["crate::bus::", "bus::"];

/// The transport is reached through the bus module and nowhere else.
pub(super) fn raw_transport_stays_inside_the_bus(subject: &Subject) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for (path, source) in judged_sources(subject)? {
        if path.starts_with(BUS_MODULE) {
            continue;
        }
        for (number, line) in numbered(&source) {
            if TRANSPORT_TOKENS.iter().any(|token| line.contains(token)) {
                violations.push(Violation::new(format!(
                    "{path}:{number} names Zenoh directly; the typed bus vocabulary in \
                     `{BUS_MODULE}` is the one place that speaks to the transport"
                )));
            }
        }
    }
    Ok(violations)
}

/// No endpoint and no topic key is written out by hand.
pub(super) fn endpoints_and_topics_are_never_authored(subject: &Subject) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for (path, source) in judged_sources(subject)? {
        let test_code = if is_test_file(&path) {
            vec![true; source.lines().count()]
        } else {
            test_code_lines(&source)
        };
        for (number, line) in numbered(&source) {
            // Test code may bind stand-in endpoints and pin rendered keys: the
            // bus is the ABI floor and must be testable without the api tree
            // above it, and a pinned literal is what notices a module rename.
            if test_code.get(number - 1) == Some(&true) {
                continue;
            }
            if path != ENDPOINT_TREE && declares_an_endpoint(line) {
                violations.push(Violation::new(format!(
                    "{path}:{number} implements an endpoint by hand; `{ENDPOINT_TREE}` declares \
                     every endpoint, and its declaration is what emits the key, the semantics and \
                     the compatibility record"
                )));
            }
            // The bus module owns key grammar: the liveliness and presence keys
            // and the execution roots a composed topic hangs under are stated
            // there because that is where they are defined. A literal in it is
            // the owner spelling its own key, not a second copy of the tree.
            if path.starts_with(BUS_MODULE) {
                continue;
            }
            if path == PRESENCE_LEASE.0 && line.contains(PRESENCE_LEASE.1) {
                continue;
            }
            if let Some(root) = authored_topic_root(line) {
                violations.push(Violation::new(format!(
                    "{path}:{number} authors a {}… key; a topic is the family root plus the \
                     module path plus the endpoint leaf, and is never spelled out",
                    root.trim_start_matches('"')
                )));
            }
        }
    }
    Ok(violations)
}

/// Every committed Rust source file these rules judge, with its content.
fn judged_sources(subject: &Subject) -> Result<Vec<(String, String)>> {
    let mut sources = Vec::new();
    for relative in tracked_source::files(&subject.root)? {
        let Some(path) = relative.to_str() else {
            continue;
        };
        if !is_judged(path) {
            continue;
        }
        let source = fs::read_to_string(subject.root.join(&relative))
            .with_context(|| format!("failed to read {path}"))?;
        sources.push((path.to_owned(), source));
    }
    Ok(sources)
}

/// Whether a workspace-relative path is one this pair of rules reads at all.
fn is_judged(path: &str) -> bool {
    path.ends_with(".rs")
        && !UNJUDGED_TREES
            .iter()
            .any(|unjudged| path.starts_with(unjudged))
}

/// The family root of the topic key one line authors, if it authors one.
///
/// [`ASSET_TREE_ROOT`] is read past rather than reported, so a line holding both
/// an asset path and a real key is still a finding.
fn authored_topic_root(line: &str) -> Option<&'static str> {
    TOPIC_ROOTS.into_iter().find(|root| {
        line.match_indices(root)
            .any(|(start, _)| !line[start..].starts_with(ASSET_TREE_ROOT))
    })
}

/// One source's lines, numbered from one.
fn numbered(source: &str) -> impl Iterator<Item = (usize, &str)> {
    source
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line))
}

/// Whether a line implements one of the endpoint traits.
///
/// `impl` may carry a generic parameter list and the trait may be qualified, so
/// the shapes accepted are `impl Endpoint for`, `impl<…> bus::Endpoint for` and
/// everything between. The trait name has to end where it is written, which is
/// what keeps `impl EndpointDescriptor for` out.
fn declares_an_endpoint(line: &str) -> bool {
    line.match_indices("impl").any(|(start, _)| {
        let mut rest = &line[start + "impl".len()..];
        if let Some(generics) = rest.strip_prefix('<') {
            let Some((_, tail)) = generics.split_once('>') else {
                return false;
            };
            rest = tail;
        }
        if !rest.starts_with(char::is_whitespace) {
            return false;
        }
        let rest = rest.trim_start();
        let rest = ENDPOINT_TRAIT_PATHS
            .iter()
            .find_map(|qualifier| rest.strip_prefix(qualifier))
            .unwrap_or(rest);
        ENDPOINT_TRAITS
            .iter()
            .find_map(|trait_name| rest.strip_prefix(trait_name))
            .is_some_and(|rest| {
                rest.starts_with(char::is_whitespace) && rest.trim_start().starts_with("for")
            })
    })
}

/// Whether a whole file is test code: an integration test under a `tests/`
/// directory, a unit-test module split out into its own `*_tests.rs` beside
/// the code it proves, or a `test_support.rs` a parent declares under
/// `#[cfg(test)]` (the one shape this file-local heuristic cannot see).
///
/// A test that names a key is stating the exact string the tree is supposed to
/// produce, which is the opposite of authoring one: it is the pin that would
/// notice a module rename.
fn is_test_file(path: &str) -> bool {
    path.split('/').any(|segment| segment == "tests")
        || path.ends_with("_tests.rs")
        || path.ends_with("/test_support.rs")
}

/// Which lines of `source` are test code, by brace depth from a `#[cfg(test)]`
/// module.
///
/// A heuristic, deliberately: a full parse would be a second Rust front end, and
/// what this has to get right is the one shape the repository uses - a
/// `#[cfg(test)] mod` whose braces balance. A file it mis-reads reports a
/// finding a human then looks at, which is the safe direction to be wrong in.
fn test_code_lines(source: &str) -> Vec<bool> {
    let mut inside = Vec::new();
    let (mut depth, mut armed, mut opened_at) = (0_isize, false, None);
    for line in source.lines() {
        let trimmed = line.trim_start();
        if opened_at.is_none() {
            if trimmed.starts_with("#[cfg(test)]") {
                armed = true;
            } else if armed && !trimmed.is_empty() && !trimmed.starts_with(['#', '/']) {
                if trimmed.starts_with("mod ") || trimmed.starts_with("pub mod ") {
                    opened_at = Some(depth);
                }
                armed = false;
            }
        }
        depth += brace_balance(line);
        inside.push(opened_at.is_some());
        if opened_at.is_some_and(|start| depth <= start) {
            opened_at = None;
        }
    }
    inside
}

/// The brace balance of one line, ignoring braces in quoted text and after a
/// line comment.
fn brace_balance(line: &str) -> isize {
    let (mut balance, mut quoted, mut escaped) = (0, false, false);
    for (index, character) in line.char_indices() {
        if !quoted && line[index..].starts_with("//") {
            break;
        }
        match character {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '{' if !quoted => balance += 1,
            '}' if !quoted => balance -= 1,
            _ => {}
        }
    }
    balance
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declaration shapes an endpoint can be written in, and the ones that
    /// only look like it.
    #[test]
    fn a_hand_written_endpoint_is_recognized_in_every_impl_shape() {
        for declared in [
            "impl Endpoint for DriveState {}",
            "impl<T> Endpoint for Bound<T> {}",
            "impl<T: Payload> crate::bus::Endpoint for T {}",
            "unsafe impl bus::QueryEndpoint for Connect {}",
            "    impl QueryEndpoint for BundleGet {",
        ] {
            assert!(declares_an_endpoint(declared), "{declared}");
        }
        for clean in [
            "impl EndpointDescriptor for DriveState {}",
            "impl Endpoint {}",
            "impl DriveState {}",
            "impl Display for Endpoint {}",
            "let endpoints = simple_endpoints_for(&tree);",
        ] {
            assert!(!declares_an_endpoint(clean), "{clean}");
        }
    }

    /// The test-code heuristic has to see the shape the repository actually
    /// uses, and has to stop at the end of it: a literal after the module is
    /// production source again.
    #[test]
    fn a_cfg_test_module_marks_its_own_lines_and_no_others() {
        let source = concat!(
            "const KEY: &str = \"robot/drive/state\";\n", // 1: production
            "#[cfg(test)]\n",                             // 2
            "mod tests {\n",                              // 3
            "    fn probe() -> &'static str {\n",         // 4
            "        \"robot/test/state\"\n",             // 5
            "    }\n",                                    // 6
            "}\n",                                        // 7
            "fn after() -> &'static str { \"runtime/logs\" }\n", // 8
        );
        assert_eq!(
            test_code_lines(source),
            [false, false, true, true, true, true, true, false]
        );
    }

    /// A `#[cfg(test)]` on something that is not a module arms nothing, so a
    /// real module further down is not swept in with it.
    #[test]
    fn a_cfg_test_item_that_is_not_a_module_opens_nothing() {
        let source = concat!(
            "#[cfg(test)]\n",                       // 1
            "use crate::testing::Clock;\n",         // 2
            "mod live {\n",                         // 3
            "    const KEY: &str = \"robot/a\";\n", // 4
            "}\n",                                  // 5
        );
        assert_eq!(test_code_lines(source), [false, false, false, false, false]);
    }

    /// A brace inside a string literal or a comment is text, not structure, so
    /// it cannot close a test module early.
    #[test]
    fn braces_in_literals_and_comments_do_not_move_the_depth() {
        assert_eq!(brace_balance("    let json = \"{\";"), 0);
        assert_eq!(brace_balance("    // closes with }"), 0);
        assert_eq!(brace_balance("    let escaped = \"\\\"{\";"), 0);
        assert_eq!(brace_balance("    fn open() {"), 1);
        assert_eq!(brace_balance("}"), -1);
    }

    /// The three whole-file shapes of test code, and the production modules
    /// whose names only look like them.
    #[test]
    fn a_test_file_is_recognized_by_where_it_sits_or_what_it_is_called() {
        assert!(is_test_file("phoxal/tests/api/topic_builder.rs"));
        assert!(is_test_file("crates/fixture/tests/staging.rs"));
        assert!(is_test_file("phoxal/src/bundle/bundle_boundary_tests.rs"));
        assert!(is_test_file("phoxal/src/bus/test_support.rs"));

        assert!(!is_test_file("phoxal/src/testing.rs"));
        assert!(!is_test_file("phoxal/src/bus/my_test_support.rs"));
        assert!(!is_test_file("phoxal/src/participant/query.rs"));
    }

    /// The runner and the fixture crate are out of scope for a stated reason,
    /// and everything that can actually reach a bus is in scope.
    #[test]
    fn the_runner_and_the_fixture_crate_are_the_trees_these_rules_do_not_read() {
        assert!(is_judged("phoxal/src/participant/query.rs"));
        assert!(is_judged("phoxal/src/session/connection.rs"));
        assert!(is_judged("services/drive/src/drive.rs"));
        assert!(is_judged("crates/macros/src/lib.rs"));
        assert!(!is_judged("xtask/src/rehearsal.rs"));
        assert!(!is_judged("crates/fixture/src/lib.rs"));
        assert!(!is_judged("phoxal/Cargo.toml"));
    }

    /// The bundle's asset tree is rooted at `robot/` too, and only its meaning
    /// separates it from the robot family root. A line that holds both is still
    /// a finding: reading past the asset root is not a licence for the key
    /// beside it.
    #[test]
    fn the_bundle_asset_root_is_read_past_and_nothing_else_is() {
        for allowed in [
            "        collect(project_root, \"robot/meshes\")?;",
            "        None => format!(\"robot/meshes/{relative}\"),",
        ] {
            assert_eq!(authored_topic_root(allowed), None, "{allowed}");
        }
        for authored in [
            "const KEY: &str = \"robot/drive/state\";",
            "    let asset = (\"robot/meshes/base.stl\", \"robot/drive/state\");",
            "        \"runtime/private/diagnostic\"",
            "pub const PRESENCE_KEY: &str = \"supervisor/presence\";",
        ] {
            assert!(authored_topic_root(authored).is_some(), "{authored}");
        }
    }

    /// The endpoint tree is inside the module that owns the transport, which is
    /// what makes one exemption enough for both rules.
    #[test]
    fn the_endpoint_tree_lives_in_the_bus_module() {
        assert!(ENDPOINT_TREE.starts_with(BUS_MODULE));
    }
}
