//! The frozen-version release-PR guard (phoxal-api-refactor companion doc,
//! "Frozen-contract enforcement").
//!
//! A released (non-`preview`) `version vN { … }` span in
//! `phoxal-api/src/lib.rs` is immutable. The published crate's
//! `.cargo_vcs_info.json` identifies the exact registry baseline commit; this
//! check diffs every `version` span at that commit against the candidate
//! revision byte-for-byte and fails if a frozen span moved, was demoted back
//! to `preview`, or disappeared.
//!
//! # It never executes the candidate revision's code
//!
//! In CI this runs under a `pull_request_target` trigger (so it fires on the
//! release-plz bot PR regardless of author), which carries the elevated
//! base-repo `GITHUB_TOKEN`. It therefore MUST NOT compile or run any code from
//! the PR head - on a public repo that is a token-exfiltration hole ("Pwn
//! Request"). The workflow checks out the trusted BASE ref (never the head sha),
//! so the `cargo xtask` binary that runs is the base repo's; the candidate
//! (head) revision of `phoxal-api/src/lib.rs` is read ONLY as text via `git
//! show <head-sha>:<path>` (`--head-sha`), never checked out over the working
//! tree, never fed to `rustc`/`build.rs`/a proc-macro. `syn::parse_file` here
//! parses that text as data; it does not execute it.
//!
//! Deliberately not a full `syn`-AST diff (RECONCILIATION correction #14):
//! `syn::parse_file` locates the one production `phoxal_api_tree!` invocation
//! and hands back its token stream with real source spans (`proc-macro2`'s
//! `span-locations` feature), which is enough to slice each version's exact
//! source text out of both file revisions and string-compare it - no
//! semantic/whitespace-insensitive comparison, no promotion policy beyond
//! "frozen means byte-identical".
//!
//! Before the first release, callers omit `--baseline-sha` and this gracefully
//! no-ops rather than failing a crate that has no registry baseline.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use proc_macro2::{Delimiter, LineColumn, Span, TokenStream, TokenTree};

use crate::workspace::Workspace;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The phoxal-api DSL source file, relative to the workspace root.
    #[arg(long, value_name = "PATH", default_value = "phoxal-api/src/lib.rs")]
    pub lib_path: PathBuf,
    /// Commit recorded in the published phoxal-api crate's
    /// `.cargo_vcs_info.json`. CI obtains it from crates.io without executing
    /// package code. Omit only before the first registry release.
    #[arg(long, value_name = "SHA")]
    pub baseline_sha: Option<String>,
    /// The candidate (PR head) commit whose `lib_path` is checked, read as text
    /// via `git show <head-sha>:<lib_path>` - never checked out or compiled.
    /// Set by the CI workflow (which runs the TRUSTED base-ref xtask, not the
    /// PR's). Omit for a local dev run, which falls back to the working-tree
    /// file.
    #[arg(long, value_name = "SHA")]
    pub head_sha: Option<String>,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;

    let Some(baseline_sha) = args.baseline_sha else {
        println!("frozen-version check: no registry baseline; nothing to check");
        return Ok(());
    };

    let baseline_source = git_show_commit(workspace.root(), &baseline_sha, &args.lib_path)?;
    // The candidate side is read as DATA, never executed: from the PR head
    // commit via `git show` under CI (trusted base-ref xtask, untrusted head
    // *content* only), or the local working tree for a dev run.
    let (current_source, candidate_desc) = match &args.head_sha {
        Some(head_sha) => (
            git_show_commit(workspace.root(), head_sha, &args.lib_path)?,
            format!("commit {head_sha}"),
        ),
        None => {
            let lib_path = workspace.root().join(&args.lib_path);
            let source = fs::read_to_string(&lib_path)
                .with_context(|| format!("failed to read {}", lib_path.display()))?;
            (source, "the working tree".to_string())
        }
    };

    let baseline_desc = format!("registry commit {baseline_sha}");
    let violations = frozen_version_violations(&baseline_desc, &baseline_source, &current_source)
        .with_context(|| {
        format!(
            "failed to compare {} at {baseline_desc} against {candidate_desc}",
            args.lib_path.display()
        )
    })?;

    if !violations.is_empty() {
        bail!(
            "frozen-version check failed ({} version(s) changed since {baseline_desc}):\n{}",
            violations.len(),
            violations.join("\n")
        );
    }

    println!("frozen-version check passed: no released version changed since {baseline_desc}");
    Ok(())
}

fn frozen_version_violations(
    baseline_desc: &str,
    baseline_source: &str,
    current_source: &str,
) -> Result<Vec<String>> {
    let baseline_spans = parse_version_spans(baseline_source)
        .context("failed to parse the baseline revision's phoxal_api_tree! invocation")?;
    let current_spans = parse_version_spans(current_source)
        .context("failed to parse the current phoxal_api_tree! invocation")?;

    // One-time namespace migration: releases from before the conventional
    // stable-vN/preview-vN lifecycle may use non-vN names. Accept their removal
    // only when the candidate contains a byte-equivalent stable `v1`, an
    // explicit preview `v2`, and no remaining non-conventional version name.
    // Once a `vN` release is the registry baseline this branch is unreachable
    // and the normal frozen-span rule below applies unchanged.
    let legacy_stable = baseline_spans
        .iter()
        .find(|span| !span.is_preview && !is_conventional_version(&span.name));
    let legacy_namespace_migration = legacy_stable.is_some()
        && current_spans.len() == 2
        && current_spans[0].name == "v1"
        && !current_spans[0].is_preview
        && current_spans[1].name == "v2"
        && current_spans[1].is_preview;

    let mut violations = Vec::new();
    if let Some(legacy_stable) = legacy_stable.filter(|_| legacy_namespace_migration) {
        let expected_v2 = baseline_spans
            .iter()
            .filter(|span| span.name != legacy_stable.name)
            .fold(BTreeMap::new(), |mut contracts, span| {
                // Later versions replace an earlier declaration of the same
                // contract path (notably simulation::Clock).
                contracts.extend(contract_surface(&span.tokens));
                contracts
            });
        let current_v2 = current_spans
            .iter()
            .find(|span| span.name == "v2")
            .expect("migration predicate requires v2");
        if contract_surface(&current_v2.tokens) != expected_v2 {
            violations.push(
                "  - preview 'v2' must preserve the complete latest contract surface from the pre-vN versions"
                    .to_string(),
            );
        }
    }

    let current_previews: Vec<_> = current_spans
        .iter()
        .filter(|span| span.is_preview)
        .collect();
    if current_previews.len() > 1 {
        violations.push(
            "  - only one API version may be preview at a time; evolve the active preview in place"
                .to_string(),
        );
    }
    if let Some(preview) = current_previews.first() {
        let max_stable = current_spans
            .iter()
            .filter(|span| !span.is_preview)
            .filter_map(|span| version_number(&span.name))
            .max();
        if current_spans
            .last()
            .is_some_and(|span| span.name != preview.name)
        {
            violations.push(format!(
                "  - preview version '{}' must be the final version block",
                preview.name
            ));
        }
        if let Some(expected_preview) = max_stable.and_then(|number| number.checked_add(1))
            && version_number(&preview.name) != Some(expected_preview)
        {
            violations.push(format!(
                "  - preview version '{}' must remain the next version 'v{expected_preview}' after the latest stable version",
                preview.name
            ));
        }
    }

    if !legacy_namespace_migration {
        for baseline_preview in baseline_spans.iter().filter(|span| span.is_preview) {
            if !current_spans
                .iter()
                .any(|span| span.name == baseline_preview.name)
            {
                violations.push(format!(
                    "  - preview version '{}' existed at {baseline_desc}; evolve it in place or promote that same version before starting another preview",
                    baseline_preview.name
                ));
            }
        }
        for current_stable in current_spans.iter().filter(|span| !span.is_preview) {
            if !baseline_spans
                .iter()
                .any(|span| span.name == current_stable.name)
            {
                violations.push(format!(
                    "  - stable version '{}' did not exist at {baseline_desc}; a new version must enter preview before promotion",
                    current_stable.name
                ));
            }
        }
    }

    for baseline in baseline_spans.iter().filter(|span| !span.is_preview) {
        match current_spans.iter().find(|span| span.name == baseline.name) {
            None if legacy_namespace_migration
                && legacy_stable.is_some_and(|stable| stable.name == baseline.name) =>
            {
                let current_v1 = current_spans
                    .iter()
                    .find(|span| span.name == "v1")
                    .expect("migration predicate requires v1");
                let migrated_v1 =
                    baseline
                        .text
                        .replacen(&format!("version {}", baseline.name), "version v1", 1);
                if current_v1.text != migrated_v1 {
                    violations.push(
                        "  - the first pre-vN stable version must move to stable 'v1' without changing its frozen contract tree"
                            .to_string(),
                    );
                }
            }
            None if legacy_namespace_migration && !is_conventional_version(&baseline.name) => {}
            None => violations.push(format!(
                "  - version '{}' was released (frozen) at {baseline_desc} but is missing now",
                baseline.name
            )),
            Some(current) if current.is_preview => violations.push(format!(
                "  - version '{}' was released (frozen) at {baseline_desc} but is now marked \
                 `preview`",
                baseline.name
            )),
            Some(current) if current.text != baseline.text => violations.push(format!(
                "  - version '{}' was released (frozen) at {baseline_desc} and must not \
                 change; make the change in the active preview version instead",
                baseline.name
            )),
            Some(_) => {}
        }
    }
    Ok(violations)
}

fn is_conventional_version(name: &str) -> bool {
    version_number(name).is_some()
}

fn version_number(name: &str) -> Option<u64> {
    let digits = name.strip_prefix('v')?;
    if digits.is_empty()
        || digits == "0"
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    digits.parse().ok()
}

/// Reads one file's content at a given commit-ish as text (`git show
/// <rev>:<path>`). This is a pure read - it never touches the working tree, so
/// the candidate (PR head) revision is inspected as data without checking it
/// out or compiling it (see the module docs on why that matters under
/// `pull_request_target`).
fn git_show_commit(root: &Path, rev: &str, relative_path: &Path) -> Result<String> {
    let spec = format!("{rev}:{}", relative_path.display());
    let output = Command::new("git")
        .args(["show", &spec])
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to spawn git show {spec}"))?;
    if !output.status.success() {
        bail!(
            "git show {spec} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).with_context(|| format!("{spec} is not UTF-8"))
}

/// One `(preview)? version <ident> { … }` block, with its exact source text
/// (including the leading `preview` keyword when present, so a frozen->preview
/// demotion shows up as a text diff too, belt-and-suspenders alongside the
/// explicit `is_preview` check).
struct VersionSpan {
    name: String,
    is_preview: bool,
    text: String,
    tokens: TokenStream,
}

/// Canonical contract/type declarations in one version, keyed by node path.
/// Comments and formatting are intentionally ignored; body fields, enum
/// variants, and topic role/type declarations remain in the token strings.
fn contract_surface(tokens: &TokenStream) -> BTreeMap<String, String> {
    fn collect(
        tokens: TokenStream,
        path: &mut Vec<String>,
        contracts: &mut BTreeMap<String, String>,
    ) {
        let trees: Vec<TokenTree> = tokens.into_iter().collect();
        let mut index = 0;
        let mut attributes = Vec::new();
        while index < trees.len() {
            if matches!(trees.get(index), Some(TokenTree::Punct(punct)) if punct.as_char() == '#')
                && let Some(TokenTree::Group(attribute)) = trees.get(index + 1)
                && attribute.delimiter() == Delimiter::Bracket
            {
                let attribute_tokens = attribute.stream().to_string();
                if !attribute_tokens.starts_with("doc =") {
                    attributes.push(format!("#[{attribute_tokens}]"));
                }
                index += 2;
                continue;
            }

            let TokenTree::Ident(ident) = &trees[index] else {
                attributes.clear();
                index += 1;
                continue;
            };
            let word = ident.to_string();

            if word == "struct" || word == "enum" {
                let Some(TokenTree::Ident(name)) = trees.get(index + 1) else {
                    index += 1;
                    continue;
                };
                let Some((end, group)) =
                    trees[index + 2..]
                        .iter()
                        .enumerate()
                        .find_map(|(offset, tree)| match tree {
                            TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => {
                                Some((index + 2 + offset, group))
                            }
                            _ => None,
                        })
                else {
                    index += 1;
                    continue;
                };
                let mut key_parts = path.clone();
                key_parts.push(name.to_string());
                let key = key_parts.join("::");
                contracts.insert(
                    key,
                    format!(
                        "{} {word} {name} {{ {} }}",
                        attributes.join(" "),
                        group.stream()
                    ),
                );
                attributes.clear();
                index = end + 1;
                continue;
            }

            if word == "topic" {
                let Some(leaf) = trees.get(index + 1) else {
                    index += 1;
                    continue;
                };
                let end = trees[index..]
                    .iter()
                    .position(
                        |tree| matches!(tree, TokenTree::Punct(punct) if punct.as_char() == ';'),
                    )
                    .map_or(trees.len() - 1, |offset| index + offset);
                let declaration = trees[index..=end]
                    .iter()
                    .cloned()
                    .collect::<TokenStream>()
                    .to_string();
                contracts.insert(format!("{}::topic::{leaf}", path.join("::")), declaration);
                attributes.clear();
                index = end + 1;
                continue;
            }

            let mut body_index = index + 1;
            let mut path_segment = word.clone();
            if matches!(trees.get(body_index), Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Parenthesis)
            {
                let TokenTree::Group(parameters) = &trees[body_index] else {
                    unreachable!("the matches! above requires a group");
                };
                path_segment.push('(');
                path_segment.push_str(&parameters.stream().to_string());
                path_segment.push(')');
                body_index += 1;
            }
            if let Some(TokenTree::Group(group)) = trees.get(body_index)
                && group.delimiter() == Delimiter::Brace
            {
                path.push(path_segment);
                collect(group.stream(), path, contracts);
                path.pop();
                attributes.clear();
                index = body_index + 1;
                continue;
            }

            attributes.clear();
            index += 1;
        }
    }

    let mut contracts = BTreeMap::new();
    collect(tokens.clone(), &mut Vec::new(), &mut contracts);
    contracts
}

fn parse_version_spans(source: &str) -> Result<Vec<VersionSpan>> {
    let file = syn::parse_file(source).context("not valid Rust source")?;
    let tokens = find_tree_invocation_tokens(&file)?;
    scan_version_spans(source, tokens)
}

fn find_tree_invocation_tokens(file: &syn::File) -> Result<TokenStream> {
    let mut found = Vec::new();
    for item in &file.items {
        if let syn::Item::Macro(item_macro) = item {
            if item_macro.mac.path.is_ident("phoxal_api_tree") {
                found.push(item_macro.mac.tokens.clone());
            }
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => bail!("expected exactly one crate-root `phoxal_api_tree!` invocation, found none"),
        count => {
            bail!("expected exactly one crate-root `phoxal_api_tree!` invocation, found {count}")
        }
    }
}

fn scan_version_spans(source: &str, tokens: TokenStream) -> Result<Vec<VersionSpan>> {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let mut spans = Vec::new();
    let mut index = 0;

    while index < trees.len() {
        let start_index = index;
        let mut is_preview = false;
        if let TokenTree::Ident(ident) = &trees[index] {
            if ident == "preview" {
                is_preview = true;
                index += 1;
            }
        }

        let version_kw = trees
            .get(index)
            .context("expected a `version` keyword but the invocation ended")?;
        let TokenTree::Ident(version_ident) = version_kw else {
            bail!("expected `version` keyword, found `{version_kw}`");
        };
        if version_ident != "version" {
            bail!("expected `version` keyword, found `{version_ident}`");
        }
        index += 1;

        let name_tt = trees
            .get(index)
            .context("expected a version name after `version`")?;
        let TokenTree::Ident(name_ident) = name_tt else {
            bail!("expected a version name identifier after `version`, found `{name_tt}`");
        };
        let name = name_ident.to_string();
        index += 1;

        let group_tt = trees
            .get(index)
            .with_context(|| format!("expected a `{{ ... }}` body for version '{name}'"))?;
        let TokenTree::Group(group) = group_tt else {
            bail!("expected a brace-delimited body for version '{name}', found `{group_tt}`");
        };
        if group.delimiter() != Delimiter::Brace {
            bail!("expected a brace-delimited body for version '{name}'");
        }

        let start_byte = line_col_to_byte(source, trees[start_index].span().start())?;
        let end_byte = line_col_to_byte(source, group.span_close().end())?;
        let text = source
            .get(start_byte..end_byte)
            .with_context(|| format!("computed span for version '{name}' was out of bounds"))?
            .to_string();

        spans.push(VersionSpan {
            name,
            is_preview,
            text,
            tokens: group.stream(),
        });
        index += 1;
    }

    Ok(spans)
}

fn line_col_to_byte(source: &str, position: LineColumn) -> Result<usize> {
    let mut offset = 0usize;
    for (zero_based_line, line) in source.split_inclusive('\n').enumerate() {
        if zero_based_line + 1 == position.line {
            let column_bytes: usize = line.chars().take(position.column).map(char::len_utf8).sum();
            return Ok(offset + column_bytes);
        }
        offset += line.len();
    }
    bail!(
        "span line {} is out of range for a {}-line source (spans require proc-macro2's \
         span-locations feature)",
        position.line,
        source.lines().count()
    )
}

/// Never constructed; documents why [`Span`] must carry real locations here
/// (this module needs `proc-macro2`'s `span-locations` feature, enabled on
/// `xtask`'s dependency - see `xtask/Cargo.toml`).
#[allow(dead_code)]
fn _assert_span_has_locations(span: Span) -> LineColumn {
    span.start()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(body: &str) -> String {
        format!(
            "//! doc comment with braces like {{instance}} must not confuse the scanner\n\
             use phoxal_macros::phoxal_api_tree;\n\n\
             phoxal_api_tree! {{\n{body}\n}}\n"
        )
    }

    #[test]
    fn parses_a_single_stable_version() -> Result<()> {
        let source = wrap(
            "    version v1 {\n        drive {\n            /// docs with a { brace } too\n            struct Target { x: f32 }\n            topic target: command Target;\n        }\n    }",
        );
        let spans = parse_version_spans(&source)?;
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "v1");
        assert!(!spans[0].is_preview);
        assert!(spans[0].text.starts_with("version v1"));
        assert!(spans[0].text.trim_end().ends_with('}'));
        Ok(())
    }

    #[test]
    fn parses_a_stable_and_a_preview_version() -> Result<()> {
        let source = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        drive { struct Target { x: f32, y: f32 } topic target: command Target; }\n    }",
        );
        let spans = parse_version_spans(&source)?;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].name, "v1");
        assert!(!spans[0].is_preview);
        assert_eq!(spans[1].name, "v2");
        assert!(spans[1].is_preview);
        assert!(spans[1].text.starts_with("preview version v2"));
        Ok(())
    }

    #[test]
    fn unchanged_frozen_version_passes() -> Result<()> {
        let baseline = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = baseline.clone();
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(violations.is_empty(), "{violations:?}");
        Ok(())
    }

    #[test]
    fn editing_a_frozen_version_fails() -> Result<()> {
        let baseline = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f64 } topic target: command Target; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("v1"));
        assert!(violations[0].contains("must not change"));
        Ok(())
    }

    #[test]
    fn demoting_a_frozen_version_to_preview_fails() -> Result<()> {
        let baseline = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = wrap(
            "    preview version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("preview"));
        Ok(())
    }

    #[test]
    fn adding_a_new_version_does_not_fail() -> Result<()> {
        let baseline = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        drive { struct Target { x: f32, y: f32 } topic target: command Target; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(violations.is_empty(), "{violations:?}");
        Ok(())
    }

    #[test]
    fn pre_vn_versions_migrate_to_stable_v1_and_preview_v2() -> Result<()> {
        let baseline = wrap(
            "    version release_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version release_7 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(violations.is_empty(), "{violations:?}");
        Ok(())
    }

    #[test]
    fn pre_vn_migration_rejects_changes_to_the_v1_contract_tree() -> Result<()> {
        let baseline = wrap(
            "    version release_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version release_7 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f64 } topic target: command Target; }\n    }\n    preview version v2 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("without changing"));
        Ok(())
    }

    #[test]
    fn pre_vn_migration_rejects_an_incomplete_v2_contract_surface() -> Result<()> {
        let baseline = wrap(
            "    version release_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version release_7 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {}",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("complete latest contract surface"));
        Ok(())
    }

    #[test]
    fn pre_vn_migration_uses_the_latest_shape_for_a_redeclared_contract() -> Result<()> {
        let baseline = wrap(
            "    version release_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version release_9 {\n        simulation { struct Clock { now_ns: u64, running: bool } topic clock: state Clock; }\n    }\n    version release_10 {\n        simulation { struct Clock { now_ns: u64, step: u64 } topic clock: state Clock; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        simulation { struct Clock { now_ns: u64, step: u64 } topic clock: state Clock; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(violations.is_empty(), "{violations:?}");
        Ok(())
    }

    #[test]
    fn pre_vn_migration_rejects_an_obsolete_redeclared_contract_shape() -> Result<()> {
        let baseline = wrap(
            "    version release_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version release_9 {\n        simulation { struct Clock { now_ns: u64, running: bool } topic clock: state Clock; }\n    }\n    version release_10 {\n        simulation { struct Clock { now_ns: u64, step: u64 } topic clock: state Clock; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        simulation { struct Clock { now_ns: u64, running: bool } topic clock: state Clock; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("complete latest contract surface"));
        Ok(())
    }

    #[test]
    fn pre_vn_migration_rejects_changed_wire_attributes_in_v2() -> Result<()> {
        let baseline = wrap(
            "    version release_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version release_7 {\n        battery { #[serde(rename_all = \"snake_case\")] enum State { Charging, Empty } topic state: state State; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        battery { #[serde(rename_all = \"camelCase\")] enum State { Charging, Empty } topic state: state State; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("complete latest contract surface"));
        Ok(())
    }

    #[test]
    fn pre_vn_migration_ignores_changed_contract_docs_in_v2() -> Result<()> {
        let baseline = wrap(
            "    version release_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version release_7 {\n        battery { /// Old wording.\n        struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        battery { /// New preview wording.\n        struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(violations.is_empty(), "{violations:?}");
        Ok(())
    }

    #[test]
    fn pre_vn_migration_rejects_changed_node_dynamics_in_v2() -> Result<()> {
        let baseline = wrap(
            "    version release_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version release_7 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        battery(instance) { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("complete latest contract surface"));
        Ok(())
    }

    #[test]
    fn pre_vn_migration_requires_preview_v2() -> Result<()> {
        let baseline = wrap(
            "    version release_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("missing"))
        );
        Ok(())
    }

    #[test]
    fn pre_vn_migration_rejects_an_extra_version() -> Result<()> {
        let baseline = wrap(
            "    version release_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version release_7 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }\n    preview version v3 {}",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(!violations.is_empty());
        Ok(())
    }

    #[test]
    fn published_preview_must_evolve_under_the_same_name() -> Result<()> {
        let baseline = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v3 {\n        battery { struct State { charge: f64 } topic state: state State; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(violations.iter().any(|violation| violation.contains("v2")));
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("next version 'v2'"))
        );
        Ok(())
    }

    #[test]
    fn published_preview_may_change_in_place() -> Result<()> {
        let baseline = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        battery { struct State { charge: f64, voltage: f32 } topic state: state State; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(violations.is_empty(), "{violations:?}");
        Ok(())
    }

    #[test]
    fn promoted_preview_allows_the_next_preview() -> Result<()> {
        let baseline = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version v2 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version v2 {\n        battery { struct State { charge: f64 } topic state: state State; }\n    }\n    preview version v3 {\n        battery { struct State { charge: f64, voltage: f32 } topic state: state State; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(violations.is_empty(), "{violations:?}");
        Ok(())
    }

    #[test]
    fn new_stable_version_must_have_existed_as_preview() -> Result<()> {
        let baseline = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version v2 {\n        battery { struct State { charge: f32 } topic state: state State; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("must enter preview"))
        );
        Ok(())
    }

    #[test]
    fn promoting_a_version_that_was_already_preview_at_baseline_does_not_fail() -> Result<()> {
        let baseline = wrap(
            "    preview version v2 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = wrap(
            "    version v2 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert!(violations.is_empty(), "{violations:?}");
        Ok(())
    }

    #[test]
    fn removing_a_frozen_version_fails() -> Result<()> {
        let baseline = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version v2 {\n        drive { struct Other { x: f32 } topic other: command Other; }\n    }",
        );
        let current = wrap(
            "    version v1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let violations = frozen_version_violations("registry commit abc123", &baseline, &current)?;
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("v2"));
        assert!(violations[0].contains("missing"));
        Ok(())
    }

    fn git(dir: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .with_context(|| format!("git {args:?}"))?;
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Proves the CI-safe read path: `git_show_commit` returns a file's content
    /// at a specific commit WITHOUT that commit being checked out (the working
    /// tree stays on a different, later revision the whole time). This is
    /// exactly how the workflow reads the untrusted PR-head `lib.rs` as data
    /// while the trusted base ref is what is actually checked out and run.
    #[test]
    fn git_show_commit_reads_a_non_checked_out_revision_as_data() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        let root = dir.path();
        let lib = Path::new("phoxal-api/src/lib.rs");

        git(root, &["init", "--quiet"])?;
        git(root, &["config", "user.email", "t@example.com"])?;
        git(root, &["config", "user.name", "test"])?;
        fs::create_dir_all(root.join("phoxal-api/src"))?;

        fs::write(root.join(lib), "// candidate head content\n")?;
        git(root, &["add", "."])?;
        git(root, &["commit", "--quiet", "-m", "head"])?;
        let head_sha = git(root, &["rev-parse", "HEAD"])?;

        // Move the working tree on to a LATER commit with different content, so
        // reading the head revision can only come from `git show`, never the
        // checked-out file.
        fs::write(root.join(lib), "// base ref content (checked out)\n")?;
        git(root, &["add", "."])?;
        git(root, &["commit", "--quiet", "-m", "base"])?;

        let read = git_show_commit(root, &head_sha, lib)?;
        assert_eq!(read, "// candidate head content\n");
        assert_eq!(
            fs::read_to_string(root.join(lib))?,
            "// base ref content (checked out)\n",
            "working tree must remain on the base ref"
        );
        Ok(())
    }
}
