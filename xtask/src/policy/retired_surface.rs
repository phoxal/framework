//! Zero-remnant rules for deleted source, launch and ownership surfaces.
//!
//! These checks use Git-tracked files and exact identifier tokens. Flag
//! spellings and attribute paths are matched literally because they are not
//! Rust identifiers. Prose and the generated changelog are excluded because
//! they record history rather than expose a source or runtime surface. Local
//! editor state cannot make the checks fail, and a legitimate compound
//! identifier cannot be rejected merely because it contains an old word as a
//! substring.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::tracked_source;
use super::{Subject, Violation};

/// This module, which spells the vocabulary it hunts for in full and would
/// otherwise be its own only violation. Naming the exemption as a path means it
/// stops applying the moment the module moves, and
/// [`the_self_exemption_names_this_module`] fails when it does.
const SELF: &str = "xtask/src/policy/retired_surface.rs";

/// How a retired spelling is recognized in committed source.
enum Token {
    /// A complete identifier-like token. A compound identifier that merely
    /// contains the retired word as a substring is not a remnant of it.
    Identifier(&'static str),
    /// Literal text, for spellings that are not Rust identifiers: command-line
    /// flags and attribute paths.
    Text(&'static str),
    /// Two identifier-like tokens occurring adjacently across punctuation, for
    /// a retired path through an authored document.
    Adjacent(&'static str, &'static str),
}

impl Token {
    fn occurs_in(&self, source: &str, tokens: &[&str]) -> bool {
        match self {
            Self::Identifier(wanted) => tokens.contains(wanted),
            Self::Text(wanted) => source.contains(wanted),
            Self::Adjacent(first, second) => {
                tokens.windows(2).any(|window| window == [*first, *second])
            }
        }
    }
}

impl std::fmt::Display for Token {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Identifier(token) | Self::Text(token) => formatter.write_str(token),
            Self::Adjacent(first, second) => write!(formatter, "{first}.{second}"),
        }
    }
}

/// One deleted surface, when it went, and why nothing may bring it back.
struct Retired {
    token: Token,
    /// The framework train that deleted the surface. It dates the rule, so a
    /// reader can tell a rule guarding last month's cutover from one guarding a
    /// decision the workspace has lived with for a year.
    train: &'static str,
    why: &'static str,
}

/// Every retired spelling, in one table.
///
/// The rules these replaced differed only in which words they looked for, so
/// they are rows rather than functions: a new retirement is a line here, and
/// the scan itself is written once.
const RETIRED: [Retired; 31] = [
    // Runtime identity is minted from the compiled participant record, launch
    // is one clap-only process contract, the bus supplies its clock directly,
    // and a managed task is either critical or finite.
    Retired {
        token: Token::Identifier("RobotNamespace"),
        train: "0.56.0",
        why: "a robot has one identity and no separate namespace axis",
    },
    Retired {
        token: Token::Identifier("RobotIdentity"),
        train: "0.56.0",
        why: "a robot has one identity and no separate namespace axis",
    },
    Retired {
        token: Token::Adjacent("robot", "namespace"),
        train: "0.56.0",
        why: "the authored namespace path went with the identity axis itself",
    },
    Retired {
        token: Token::Identifier("PHOXAL_NAMESPACE"),
        train: "0.54.0",
        why: "a runtime reads its identity from the compiled record, not the environment",
    },
    Retired {
        token: Token::Identifier("PHOXAL_ROBOT_ID"),
        train: "0.56.0",
        why: "a runtime reads its identity from the compiled record, not the environment",
    },
    Retired {
        token: Token::Identifier("PHOXAL_LAUNCH"),
        train: "0.56.0",
        why: "launch is one clap-only process contract with no environment channel beside it",
    },
    Retired {
        token: Token::Identifier("LaunchEnv"),
        train: "0.56.0",
        why: "launch is one clap-only process contract with no environment channel beside it",
    },
    Retired {
        token: Token::Text("--namespace"),
        train: "0.56.1",
        why: "launch is one clap-only process contract with no namespace operand",
    },
    Retired {
        token: Token::Text("--robot-id"),
        train: "0.56.1",
        why: "a runtime reads its identity from the compiled record, not from argv",
    },
    Retired {
        token: Token::Identifier("run_with_bus_clock"),
        train: "0.56.0",
        why: "the bus supplies its clock directly, so a runner takes no clock parameter",
    },
    Retired {
        token: Token::Identifier("AllowExit"),
        train: "0.56.0",
        why: "a managed task is either critical or finite; there is no third policy",
    },
    // Compatibility is one identity: the framework train version every
    // participant binary embeds, compared by the line the two trains belong to.
    // A second identity is a second answer to a question that has one.
    Retired {
        token: Token::Identifier("ParticipantSchemas"),
        train: "0.57.0",
        why: "the framework train version is the only negotiated identity",
    },
    Retired {
        token: Token::Identifier("BusAbi"),
        train: "0.57.0",
        why: "the framework train version is the only negotiated identity",
    },
    Retired {
        token: Token::Identifier("LaunchAbi"),
        train: "0.57.0",
        why: "the framework train version is the only negotiated identity",
    },
    Retired {
        token: Token::Identifier("RuntimeSchema"),
        train: "0.57.0",
        why: "the framework train version is the only negotiated identity",
    },
    Retired {
        token: Token::Identifier("version_identity"),
        train: "0.57.0",
        why: "the framework train version is the only negotiated identity",
    },
    Retired {
        token: Token::Identifier("ApiVersion"),
        train: "0.57.0",
        why: "wire contracts are grouped by semantic family and carry no revision axis",
    },
    Retired {
        token: Token::Identifier("RobotApiVersion"),
        train: "0.57.0",
        why: "wire contracts are grouped by semantic family and carry no revision axis",
    },
    Retired {
        token: Token::Identifier("UnsupportedRobotApi"),
        train: "0.57.0",
        why: "a train mismatch is reported against the framework version, not a per-api one",
    },
    // A bundle carries one document, and a role carries no topology or world
    // privilege. A table beside the manifest would be a second answer to what a
    // robot runs.
    Retired {
        token: Token::Identifier("RuntimeDocument"),
        train: "0.63.0",
        why: "the manifest is the bundle's only persisted document",
    },
    Retired {
        token: Token::Identifier("RuntimeParticipant"),
        train: "0.63.0",
        why: "the process set is derived from the manifest, never tabulated beside it",
    },
    Retired {
        token: Token::Identifier("AssetIndex"),
        train: "0.63.0",
        why: "assets are read from the bundle layout, not from a table describing it",
    },
    Retired {
        token: Token::Identifier("BinaryReference"),
        train: "0.63.0",
        why: "a launcher derives the binary from the id it launches",
    },
    Retired {
        token: Token::Identifier("ParticipantRequirement"),
        train: "0.63.0",
        why: "a role declares no topology requirement of its own",
    },
    Retired {
        token: Token::Identifier("ExecutionOrigin"),
        train: "0.63.0",
        why: "robot time zero is host boot, so a launcher states no time origin",
    },
    Retired {
        token: Token::Identifier("ParticipantClock"),
        train: "0.63.0",
        why: "simulation is a launch decision, not a per-participant clock declaration",
    },
    Retired {
        token: Token::Identifier("WorldAuthoritySurface"),
        train: "0.63.0",
        why: "the world clock is published by an external bus client, not a privileged role",
    },
    Retired {
        token: Token::Identifier("Simulator"),
        train: "0.63.0",
        why: "the simulator role is gone; a simulated world is an ordinary external client",
    },
    Retired {
        // Literal text rather than an adjacent pair: the sibling repository the
        // controller moved to is spelled `phoxal/simulator-webots`, and a pair
        // rule would read that path as this attribute.
        token: Token::Text("phoxal::simulator"),
        train: "0.63.0",
        why: "the simulator role attribute went with the role itself",
    },
    // Driving intent crosses the wire normalized, so the robot's physics stay
    // in the robot. A client that re-derives a twist from geometry it was
    // handed is a second copy of the kinematics `robot/drive` already owns.
    Retired {
        token: Token::Identifier("ManualDrive"),
        train: "0.64.0",
        why: "manual intent is normalized on the wire and carries no kinematic envelope",
    },
    Retired {
        token: Token::Identifier("manual_drive"),
        train: "0.64.0",
        why: "manual intent is normalized on the wire and carries no kinematic envelope",
    },
];

fn source_files(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(tracked_source::files(root)?
        .into_iter()
        .filter(|path| {
            matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("rs" | "toml" | "yaml" | "yml" | "json" | "json5")
            )
        })
        .collect())
}

/// Identifier-like tokens from source text; comments and strings are included
/// because a zero-remnant rule also rejects obsolete source vocabulary there.
fn identifiers(source: &str) -> impl Iterator<Item = &str> {
    source
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|token| !token.is_empty())
}

/// Whether two identifier-like tokens occur adjacently across punctuation.
fn contains_identifier_pair(source: &str, first: &str, second: &str) -> bool {
    identifiers(source)
        .collect::<Vec<_>>()
        .windows(2)
        .any(|window| window == [first, second])
}

/// Every deleted surface stays deleted.
///
/// One pass over the committed source decides the whole table: each file is
/// read and tokenized once, and every row is answered from that.
pub(super) fn retired_surfaces_stay_absent(subject: &Subject) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for relative in source_files(&subject.root)? {
        if relative == Path::new(SELF) {
            continue;
        }
        let source = read(&subject.root, &relative)?;
        let tokens = identifiers(&source).collect::<Vec<_>>();
        for retired in &RETIRED {
            if retired.token.occurs_in(&source, &tokens) {
                violations.push(Violation::new(format!(
                    "{} brings back {}, retired in {}: {}",
                    relative.display(),
                    retired.token,
                    retired.train,
                    retired.why
                )));
            }
        }
    }
    Ok(violations)
}

/// Participant kind has one process-contract owner and one private macro
/// dispatch mirror. The macro enum maps attribute roles to the contract's wire
/// variants during expansion and never becomes an authored document field.
pub(super) fn participant_kind_declarations_match_the_two_explicit_owners(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let declaration = ("enum", "ParticipantKind");
    let expected = [
        PathBuf::from("crates/macros/src/authoring.rs"),
        PathBuf::from("phoxal/src/participant/metadata/mod.rs"),
    ];
    let mut owners = Vec::new();
    for relative in source_files(&subject.root)? {
        if relative == Path::new(SELF) {
            continue;
        }
        let source = read(&subject.root, &relative)?;
        if contains_identifier_pair(&source, declaration.0, declaration.1) {
            owners.push(relative);
        }
    }
    owners.sort_unstable();

    let mut violations = Vec::new();
    for owner in &owners {
        if !expected.contains(owner) {
            violations.push(Violation::new(format!(
                "{} declares the participant-kind enum; only its two explicit owners may",
                owner.display()
            )));
        }
    }
    for owner in &expected {
        if !owners.contains(owner) {
            violations.push(Violation::new(format!(
                "{} is an explicit participant-kind owner but no longer declares the enum",
                owner.display()
            )));
        }
    }
    Ok(violations)
}

/// A source-reference type is used under its canonical name. Public type
/// aliases and renamed public re-exports would preserve an obsolete parallel
/// spelling, including restricted-visibility and multiline forms.
pub(super) fn source_reference_alias_shims_stay_absent(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let source_reference = "SourceRef";
    let mut violations = Vec::new();
    for relative in source_files(&subject.root)? {
        if relative == Path::new(SELF) {
            continue;
        }
        let source = read(&subject.root, &relative)?;
        let mut offset = 0;
        for statement in source.split_inclusive(';') {
            if let Some(item_offset) = public_source_reference_alias(statement, source_reference) {
                violations.push(Violation::new(format!(
                    "{}:{}",
                    relative.display(),
                    source[..offset + item_offset].lines().count() + 1
                )));
            }
            offset += statement.len();
        }
    }
    Ok(violations)
}

fn read(root: &Path, relative: &Path) -> Result<String> {
    fs::read_to_string(root.join(relative))
        .with_context(|| format!("failed to read {}", relative.display()))
}

/// The byte offset and keyword of a public `type` or `use` item beginning a
/// line. Same-line attributes are skipped, restricted visibility is accepted,
/// and identifiers or visibility from earlier items cannot leak into it.
fn public_item(statement: &str) -> Option<(usize, &str)> {
    let mut offset = 0;
    for line in statement.split_inclusive('\n') {
        let mut rest = line.trim_start();
        while let Some(attribute) = rest.strip_prefix("#[") {
            let (_, tail) = attribute.split_once(']')?;
            rest = tail.trim_start();
        }
        let after_public = rest.strip_prefix("pub").and_then(|rest| {
            if let Some(restricted) = rest.strip_prefix('(') {
                restricted.split_once(')').map(|(_, tail)| tail)
            } else {
                rest.chars()
                    .next()
                    .is_some_and(char::is_whitespace)
                    .then_some(rest)
            }
        });
        if let Some(keyword) = after_public
            .map(str::trim_start)
            .and_then(|rest| rest.split_whitespace().next())
            .filter(|word| matches!(*word, "type" | "use"))
        {
            let public_offset = line.find("pub")?;
            return Some((offset + public_offset, keyword));
        }
        offset += line.len();
    }
    None
}

/// The byte offset of a public alias for the canonical source-reference name
/// within one semicolon-delimited Rust chunk, or `None` when no alias exists.
fn public_source_reference_alias(statement: &str, source_reference: &str) -> Option<usize> {
    let (offset, kind) = public_item(statement)?;
    let item = &statement[offset..];
    let matched = match kind {
        "type" => {
            let name_is_alias = identifiers(item)
                .skip_while(|token| *token != "type")
                .nth(1)
                == Some(source_reference);
            let target_is_direct_path = item.split_once('=').is_some_and(|(_, target)| {
                let target = target.trim().trim_end_matches(';').trim();
                target.chars().all(|character| {
                    character.is_ascii_alphanumeric() || "_: \t\r\n".contains(character)
                }) && identifiers(target).last() == Some(source_reference)
            });
            name_is_alias || target_is_direct_path
        }
        "use" => identifiers(item)
            .collect::<Vec<_>>()
            .windows(2)
            .any(|window| window[0] == source_reference && window[1] == "as"),
        _ => false,
    };
    matched.then_some(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three match kinds are one rule's vocabulary, so the distinction
    /// between them - a whole identifier, literal text, and an adjacent pair -
    /// is pinned rather than assumed.
    #[test]
    fn a_retired_token_matches_only_its_own_spelling() {
        let tokens = |source: &'static str| identifiers(source).collect::<Vec<_>>();

        let source = "let value = RobotNamespaceLike::new();";
        assert!(!Token::Identifier("RobotNamespace").occurs_in(source, &tokens(source)));
        let source = "use crate::RobotNamespace;";
        assert!(Token::Identifier("RobotNamespace").occurs_in(source, &tokens(source)));

        let source = "run --namespace rover";
        assert!(Token::Text("--namespace").occurs_in(source, &tokens(source)));
        let source = "robot.namespace = \"rover\"";
        assert!(Token::Adjacent("robot", "namespace").occurs_in(source, &tokens(source)));
        let source = "robot.id = \"rover\"; namespace";
        assert!(!Token::Adjacent("robot", "namespace").occurs_in(source, &tokens(source)));
    }

    #[test]
    fn source_reference_alias_detection_handles_visibility_and_multiline_items() {
        let name = "SourceRef";
        assert!(
            public_source_reference_alias("pub(crate) type Ref = contract::SourceRef;", name)
                .is_some()
        );
        assert!(
            public_source_reference_alias(
                "pub use contract::{\n SourceRef as Ref, Other\n};",
                name
            )
            .is_some()
        );
        assert!(
            public_source_reference_alias(
                "#[cfg(feature = \"legacy\")]\npub type Ref = contract::SourceRef;",
                name
            )
            .is_some()
        );
        assert!(
            public_source_reference_alias(
                "#[rustfmt::skip] pub type Ref = contract::SourceRef;",
                name
            )
            .is_some()
        );
        assert!(
            public_source_reference_alias("pub use contract::{SourceRef, Other as Alias};", name)
                .is_none()
        );
        assert!(public_source_reference_alias("type Item = SourceRef;", name).is_none());
        assert!(
            public_source_reference_alias("pub type Sources = Vec<SourceRef>;", name).is_none()
        );
        assert!(public_source_reference_alias(
            "pub struct Wrapper { inner: u8 }\nimpl Trait for Wrapper {\n type Item = SourceRef;",
            name
        )
        .is_none());
        assert!(public_source_reference_alias("//! written as `pub type`", name).is_none());
        assert!(
            public_source_reference_alias(
                "// SourceRef as Ref is forbidden.\npub use contract::SourceRef;",
                name
            )
            .is_none()
        );
        assert!(public_source_reference_alias(
            "pub struct Config {\n pub r#type: String,\n}\nimpl Trait for Config {\n type Item = SourceRef;",
            name
        )
        .is_none());
    }

    /// The exemption is a path, so it stops applying the moment this module
    /// moves - and the rules would then report themselves.
    #[test]
    fn the_self_exemption_names_this_module() {
        assert!(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/policy/retired_surface.rs")
                .ends_with(SELF)
        );
    }
}
