//! Zero-remnant rules for deleted source, launch and ownership surfaces.
//!
//! These checks use Git-tracked files and exact identifier tokens. Flag
//! spellings are matched literally because hyphenated options are not Rust
//! identifiers. Prose and the generated changelog are excluded because they
//! record history rather than expose a source or runtime surface. Local editor
//! state cannot make the checks fail, and a legitimate compound identifier
//! cannot be rejected merely because it contains an old word as a substring.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use workspace_policy::{tracked_source, workspace_root};

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

/// Whether `wanted` occurs as a complete identifier-like token.
fn contains_identifier(source: &str, wanted: &str) -> bool {
    identifiers(source).any(|token| token == wanted)
}

/// Whether two identifier-like tokens occur adjacently across punctuation.
fn contains_identifier_pair(source: &str, first: &str, second: &str) -> bool {
    let tokens = identifiers(source).collect::<Vec<_>>();
    tokens.windows(2).any(|window| window == [first, second])
}

/// Runtime identity is minted from the compiled participant record, launch is
/// one clap-only process contract, the bus supplies its clock directly, and a
/// managed task is either critical or finite. Deleted parallel axes must not
/// return under their former exact identifiers or flag spellings.
#[test]
fn retired_runtime_vocabulary_stays_absent() -> Result<()> {
    let root = workspace_root()?;
    // Split literals keep this rule from becoming its own only violation.
    let identifiers = [
        ["Robot", "Namespace"].concat(),
        ["Robot", "Identity"].concat(),
        ["PHOXAL_", "NAMESPACE"].concat(),
        ["PHOXAL_", "ROBOT_ID"].concat(),
        ["PHOXAL_", "LAUNCH"].concat(),
        ["Launch", "Env"].concat(),
        ["run_with_", "bus_clock"].concat(),
        ["Allow", "Exit"].concat(),
    ];
    let flag_spellings = [["--name", "space"].concat(), ["--robot", "-id"].concat()];
    let robot_namespace = (["ro", "bot"].concat(), ["name", "space"].concat());

    let mut violations = Vec::new();
    for relative in source_files(root)? {
        let path = root.join(&relative);
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        for term in &identifiers {
            if contains_identifier(&source, term) {
                violations.push(format!("{} contains identifier {term}", relative.display()));
            }
        }
        for flag in &flag_spellings {
            if source.contains(flag) {
                violations.push(format!("{} contains flag {flag}", relative.display()));
            }
        }
        if contains_identifier_pair(&source, &robot_namespace.0, &robot_namespace.1) {
            violations.push(format!(
                "{} contains the retired authored namespace path",
                relative.display()
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "retired runtime surfaces returned:\n{}",
        violations.join("\n")
    );
    Ok(())
}

/// Participant kind has one process-contract owner and one private macro
/// dispatch mirror. The macro enum maps attribute roles to the contract's wire
/// variants during expansion and never becomes an authored document field.
#[test]
fn participant_kind_declarations_match_the_two_explicit_owners() -> Result<()> {
    let root = workspace_root()?;
    let declaration = ("enum", ["Participant", "Kind"].concat());
    let expected = [
        PathBuf::from("crates/macros/src/authoring.rs"),
        PathBuf::from("crates/runtime-contract/src/metadata.rs"),
    ];
    let mut owners = Vec::new();
    for relative in source_files(root)? {
        let source = fs::read_to_string(root.join(&relative))
            .with_context(|| format!("failed to read {}", relative.display()))?;
        if contains_identifier_pair(&source, declaration.0, &declaration.1) {
            owners.push(relative);
        }
    }
    owners.sort_unstable();
    assert_eq!(owners, expected, "participant-kind ownership drifted");
    Ok(())
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

/// A source-reference type is used under its canonical name. Public type
/// aliases and renamed public re-exports would preserve an obsolete parallel
/// spelling, including restricted-visibility and multiline forms.
#[test]
fn source_reference_alias_shims_stay_absent() -> Result<()> {
    let root = workspace_root()?;
    let source_reference = ["Source", "Ref"].concat();
    let mut violations = Vec::new();
    for relative in source_files(root)? {
        if relative.starts_with("workspace-policy") {
            // This test defines the detection vocabulary and is not a product
            // surface that can expose an alias to a consumer.
            continue;
        }
        let source = fs::read_to_string(root.join(&relative))
            .with_context(|| format!("failed to read {}", relative.display()))?;
        let mut offset = 0;
        for statement in source.split_inclusive(';') {
            if let Some(item_offset) = public_source_reference_alias(statement, &source_reference) {
                violations.push(format!(
                    "{}:{}",
                    relative.display(),
                    source[..offset + item_offset].lines().count() + 1
                ));
            }
            offset += statement.len();
        }
    }
    assert!(
        violations.is_empty(),
        "source-reference alias shims returned: {violations:?}"
    );
    Ok(())
}

mod tests {
    use super::public_source_reference_alias;

    fn source_reference() -> String {
        ["Source", "Ref"].concat()
    }

    #[test]
    fn source_reference_alias_detection_handles_visibility_and_multiline_items() {
        let name = source_reference();
        assert!(
            public_source_reference_alias("pub(crate) type Ref = contract::SourceRef;", &name)
                .is_some()
        );
        assert!(
            public_source_reference_alias(
                "pub use contract::{\n SourceRef as Ref, Other\n};",
                &name
            )
            .is_some()
        );
        assert!(
            public_source_reference_alias(
                "#[cfg(feature = \"legacy\")]\npub type Ref = contract::SourceRef;",
                &name
            )
            .is_some()
        );
        assert!(
            public_source_reference_alias(
                "#[rustfmt::skip] pub type Ref = contract::SourceRef;",
                &name
            )
            .is_some()
        );
        assert!(
            public_source_reference_alias("pub use contract::{SourceRef, Other as Alias};", &name)
                .is_none()
        );
        assert!(public_source_reference_alias("type Item = SourceRef;", &name).is_none());
        assert!(
            public_source_reference_alias("pub type Sources = Vec<SourceRef>;", &name).is_none()
        );
        assert!(public_source_reference_alias(
            "pub struct Wrapper { inner: u8 }\nimpl Trait for Wrapper {\n type Item = SourceRef;",
            &name
        )
        .is_none());
        assert!(public_source_reference_alias("//! written as `pub type`", &name).is_none());
        assert!(
            public_source_reference_alias(
                "// SourceRef as Ref is forbidden.\npub use contract::SourceRef;",
                &name
            )
            .is_none()
        );
        assert!(public_source_reference_alias(
            "pub struct Config {\n pub r#type: String,\n}\nimpl Trait for Config {\n type Item = SourceRef;",
            &name
        )
        .is_none());
    }
}
