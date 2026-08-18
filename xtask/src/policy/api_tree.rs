//! The api tree is declared, never written out.
//!
//! Two facts hold the tree together, and neither is checkable inside one crate:
//! an endpoint's family and semantics come from the `endpoints!` declaration
//! beside its payload and from nowhere else, and a family-rooted key is
//! rendered by walking the tree rather than spelled by whoever needed it. Both
//! are one edit away from being quietly worked around, so they are a gate.
//!
//! Committed test sources are outside both rules. The bus is the ABI floor and
//! has to be exercisable without the tree above it, so its unit tests declare
//! stand-in endpoints by hand and name literal keys for them; a test that pins a
//! rendered key against the literal it is expected to be is the whole point of
//! that test.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::tracked_source;
use super::{Subject, Violation};

/// This module, which spells the vocabulary it hunts for and would otherwise be
/// its own only violation.
const SELF: &str = "xtask/src/policy/api_tree.rs";

/// The one file that writes an endpoint binding: the two declarations expand
/// from here, so this is where the `impl` text lives.
const DECLARATION_SITE: &str = "phoxal/src/bus/tree.rs";

/// Where an authored family-rooted key would matter: the three families
/// themselves, the engine that walks them, and every framework consumer of the
/// walk. Authored source and fixture trees are deliberately outside it - a
/// `robot/meshes/...` asset path is not a bus key and shares nothing with one
/// but its first word.
const KEY_SCOPE: [&str; 8] = [
    "phoxal/src/api/",
    "phoxal/src/bus/",
    "phoxal/src/participant/",
    "phoxal/src/runtime/",
    "phoxal/src/supervisor/",
    "components/",
    "services/",
    "supervisor/src/",
];

/// The one authored key that is not an endpoint: the supervisor's Liveliness
/// presence lease, which is frozen as a literal on purpose and is pinned by its
/// own test not to collide with any endpoint.
const PRESENCE_LEASE: (&str, &str) = (
    "phoxal/src/supervisor/api/connect.rs",
    "supervisor/presence",
);

/// The leading segment of every key the three families declare.
const FAMILIES: [&str; 3] = ["robot", "runtime", "supervisor"];

fn source_files(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(tracked_source::files(root)?
        .into_iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("rs"))
        .collect())
}

fn read(root: &Path, relative: &Path) -> Result<String> {
    fs::read_to_string(root.join(relative))
        .with_context(|| format!("failed to read {}", relative.display()))
}

/// Whether a committed file is test source.
///
/// A file under a `tests/` tree, one whose name says it holds tests, and the
/// bus's own shared stand-in fixtures.
fn is_test_source(relative: &Path) -> bool {
    if relative
        .components()
        .any(|component| component.as_os_str() == "tests" || component.as_os_str() == "trybuild")
    {
        return true;
    }
    matches!(
        relative.file_stem().and_then(|value| value.to_str()),
        Some(stem) if stem.ends_with("_tests") || stem == "test_support"
    )
}

/// The part of a file that is not its own unit-test module.
///
/// Every unit-test module in this workspace is one `#[cfg(test)]` block at the
/// end of the file it tests, which the test-module-ownership rule already
/// enforces, so cutting at the first marker is exact rather than approximate.
fn production_source(source: &str) -> &str {
    match source.find("#[cfg(test)]") {
        Some(at) => &source[..at],
        None => source,
    }
}

/// Identifier-like tokens, in order.
fn identifiers(source: &str) -> impl Iterator<Item = &str> {
    source
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|token| !token.is_empty())
}

/// Whether one line is a hand-written endpoint binding: an `impl` whose trait
/// is `Endpoint` or `QueryEndpoint` itself, however it is qualified.
///
/// The token pair is what makes it exact. A blanket over an endpoint bound -
/// `impl<E: Endpoint<Family = Robot>> RobotEndpoint for E` - names a different
/// trait and is not one of these.
fn binds_an_endpoint(line: &str) -> bool {
    if !line.trim_start().starts_with("impl") {
        return false;
    }
    identifiers(line)
        .collect::<Vec<_>>()
        .windows(2)
        .any(|window| window == ["Endpoint", "for"] || window == ["QueryEndpoint", "for"])
}

/// Whether one string literal in `line` is a family-rooted key.
///
/// A key is only key characters all the way to its closing quote, so a
/// diagnostic message that opens with one - `"runtime/simulation/clock sample
/// has no exact production instant"` - is prose and not a key.
fn authored_keys(line: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for family in FAMILIES {
        let opening = format!("\"{family}/");
        let mut from = 0;
        while let Some(at) = line[from..].find(&opening) {
            let start = from + at + 1;
            let Some(end) = line[start..].find('"') else {
                break;
            };
            let literal = &line[start..start + end];
            if literal
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "_-/{}*".contains(character))
            {
                keys.push(literal.to_owned());
            }
            from = start + end;
        }
    }
    keys
}

/// An endpoint's family and semantics are written by its own declaration.
pub(super) fn endpoint_bindings_come_only_from_the_declaration(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for relative in source_files(&subject.root)? {
        if relative == Path::new(SELF)
            || relative == Path::new(DECLARATION_SITE)
            || is_test_source(&relative)
        {
            continue;
        }
        let source = read(&subject.root, &relative)?;
        for (index, line) in production_source(&source).lines().enumerate() {
            if binds_an_endpoint(line) {
                violations.push(Violation::new(format!(
                    "{}:{} binds an endpoint by hand; only the `endpoints!` declaration \
                     beside a payload may, so the compatibility records cannot miss one",
                    relative.display(),
                    index + 1
                )));
            }
        }
    }
    Ok(violations)
}

/// A family-rooted key is rendered by the tree, never authored.
pub(super) fn family_rooted_keys_are_rendered_not_authored(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for relative in source_files(&subject.root)? {
        if relative == Path::new(SELF) || is_test_source(&relative) {
            continue;
        }
        let Some(path) = relative.to_str() else {
            continue;
        };
        if !KEY_SCOPE.iter().any(|scope| path.starts_with(scope)) {
            continue;
        }
        let source = read(&subject.root, &relative)?;
        for (index, line) in production_source(&source).lines().enumerate() {
            for key in authored_keys(line) {
                if (path, key.as_str()) == PRESENCE_LEASE {
                    continue;
                }
                violations.push(Violation::new(format!(
                    "{}:{} authors the family-rooted key {key:?}; walk the api tree instead, \
                     so the key and its compatibility template cannot drift",
                    relative.display(),
                    index + 1
                )));
            }
        }
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule is the token pair, not the word: a blanket implementation over
    /// an endpoint bound names a different trait and is not a binding.
    #[test]
    fn only_an_impl_of_the_endpoint_trait_itself_is_a_binding() {
        assert!(binds_an_endpoint("impl crate::bus::Endpoint for Target {"));
        assert!(binds_an_endpoint(
            "        impl crate::bus::QueryEndpoint for $request {"
        ));
        assert!(binds_an_endpoint("impl Endpoint for Target {}"));

        assert!(!binds_an_endpoint(
            "impl<E: Endpoint<Family = Robot>> RobotEndpoint for E {}"
        ));
        assert!(!binds_an_endpoint(
            "impl<E: Endpoint> Clone for Subscriber<E> {"
        ));
        assert!(!binds_an_endpoint("pub trait Endpoint: Payload {"));
        assert!(!binds_an_endpoint("/// One Endpoint for each payload."));
    }

    /// A key is key characters all the way to its closing quote; prose that
    /// opens with one is a message, not a key.
    #[test]
    fn only_a_whole_key_shaped_literal_is_an_authored_key() {
        assert_eq!(
            authored_keys(r#"let key = "robot/drive/state";"#),
            ["robot/drive/state"]
        );
        assert_eq!(
            authored_keys(r#""robot/component/{instance}/motor/{capability}/command""#),
            ["robot/component/{instance}/motor/{capability}/command"]
        );
        assert!(
            authored_keys(r#"anyhow!("runtime/simulation/clock subscriber terminated")"#)
                .is_empty()
        );
        assert!(authored_keys(r#"format!("robot/meshes/{relative}.stl")"#).is_empty());
        assert!(authored_keys(r#"let path = "assets/robot/drive";"#).is_empty());
    }

    /// The unit-test module of a file is outside both rules, and so is a file
    /// that holds nothing else.
    #[test]
    fn test_source_is_outside_both_rules() {
        assert!(is_test_source(Path::new("phoxal/tests/api/tree.rs")));
        assert!(is_test_source(Path::new("phoxal/src/bus/test_support.rs")));
        assert!(is_test_source(Path::new(
            "supervisor/src/supervisor/serve/endpoint_contract_tests.rs"
        )));
        assert!(!is_test_source(Path::new("phoxal/src/api/drive.rs")));

        assert_eq!(
            production_source("fn a() {}\n#[cfg(test)]\nmod tests {\n\"robot/x\"\n}\n"),
            "fn a() {}\n"
        );
        assert_eq!(production_source("fn a() {}\n"), "fn a() {}\n");
    }
}
