//! The frozen-generation release-PR guard (phoxal-api-refactor companion doc,
//! "Frozen-contract enforcement").
//!
//! A released (non-`preview`) `version y2026_N { … }` span in
//! `phoxal-api/src/lib.rs` is immutable: once its per-package tag
//! (`phoxal-api-v{version}`, release-plz's `git_tag_name`) is cut, that span
//! must never change again - a changed contract is minted fresh in the
//! current generation instead (D1's sparse-generation model). This check is
//! the CI backstop for that rule: it diffs every `version` span that was
//! already released at the last `phoxal-api-v*` tag against the candidate
//! revision, byte-for-byte, and fails if a frozen span moved, or was demoted
//! back to `preview`, or disappeared.
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
//! `span-locations` feature), which is enough to slice each generation's exact
//! source text out of both file revisions and string-compare it - no
//! semantic/whitespace-insensitive comparison, no promotion policy beyond
//! "frozen means byte-identical".
//!
//! No `phoxal-api-v*` tag exists yet before the first release (verified at
//! Step 0); this gracefully no-ops (log + pass) in that case rather than
//! failing a repo that has never cut a release.

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
    /// Git tag glob identifying the last released phoxal-api baseline
    /// (release-plz's per-package tag format is `phoxal-api-v{version}`,
    /// `release-plz.toml`'s `git_tag_name`).
    #[arg(long, value_name = "GLOB", default_value = "phoxal-api-v*")]
    pub tag_glob: String,
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

    let Some(baseline_tag) = latest_matching_tag(workspace.root(), &args.tag_glob)? else {
        println!(
            "frozen-generation check: no tag matching '{}' found (phoxal-api has not been \
             released yet); nothing to check",
            args.tag_glob
        );
        return Ok(());
    };

    let baseline_source = git_show(workspace.root(), &baseline_tag, &args.lib_path)?;
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

    let violations = frozen_generation_violations(&baseline_tag, &baseline_source, &current_source)
        .with_context(|| {
            format!(
                "failed to compare {} at {baseline_tag} against {candidate_desc}",
                args.lib_path.display()
            )
        })?;

    if !violations.is_empty() {
        bail!(
            "frozen-generation check failed ({} generation(s) changed since {baseline_tag}):\n{}",
            violations.len(),
            violations.join("\n")
        );
    }

    println!("frozen-generation check passed: no released generation changed since {baseline_tag}");
    Ok(())
}

fn frozen_generation_violations(
    baseline_tag: &str,
    baseline_source: &str,
    current_source: &str,
) -> Result<Vec<String>> {
    let baseline_spans = parse_generation_spans(baseline_source)
        .context("failed to parse the baseline revision's phoxal_api_tree! invocation")?;
    let current_spans = parse_generation_spans(current_source)
        .context("failed to parse the current phoxal_api_tree! invocation")?;

    let mut violations = Vec::new();
    for baseline in baseline_spans.iter().filter(|span| !span.is_preview) {
        match current_spans.iter().find(|span| span.name == baseline.name) {
            None => violations.push(format!(
                "  - generation '{}' was released (frozen) at {baseline_tag} but is missing now",
                baseline.name
            )),
            Some(current) if current.is_preview => violations.push(format!(
                "  - generation '{}' was released (frozen) at {baseline_tag} but is now marked \
                 `preview`",
                baseline.name
            )),
            Some(current) if current.text != baseline.text => violations.push(format!(
                "  - generation '{}' was released (frozen) at {baseline_tag} and must not \
                 change; mint a new generation instead",
                baseline.name
            )),
            Some(_) => {}
        }
    }
    Ok(violations)
}

fn latest_matching_tag(root: &Path, glob: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["tag", "-l", glob, "--sort=-v:refname"])
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to spawn git tag -l {glob}"))?;
    if !output.status.success() {
        bail!(
            "git tag -l {glob} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8(output.stdout).context("git tag -l output was not UTF-8")?;
    Ok(stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string))
}

fn git_show(root: &Path, tag: &str, relative_path: &Path) -> Result<String> {
    git_show_commit(root, tag, relative_path)
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
struct GenerationSpan {
    name: String,
    is_preview: bool,
    text: String,
}

fn parse_generation_spans(source: &str) -> Result<Vec<GenerationSpan>> {
    let file = syn::parse_file(source).context("not valid Rust source")?;
    let tokens = find_tree_invocation_tokens(&file)?;
    scan_generation_spans(source, tokens)
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

fn scan_generation_spans(source: &str, tokens: TokenStream) -> Result<Vec<GenerationSpan>> {
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
            .context("expected a generation name after `version`")?;
        let TokenTree::Ident(name_ident) = name_tt else {
            bail!("expected a generation name identifier after `version`, found `{name_tt}`");
        };
        let name = name_ident.to_string();
        index += 1;

        let group_tt = trees
            .get(index)
            .with_context(|| format!("expected a `{{ ... }}` body for generation '{name}'"))?;
        let TokenTree::Group(group) = group_tt else {
            bail!("expected a brace-delimited body for generation '{name}', found `{group_tt}`");
        };
        if group.delimiter() != Delimiter::Brace {
            bail!("expected a brace-delimited body for generation '{name}'");
        }

        let start_byte = line_col_to_byte(source, trees[start_index].span().start())?;
        let end_byte = line_col_to_byte(source, group.span_close().end())?;
        let text = source
            .get(start_byte..end_byte)
            .with_context(|| format!("computed span for generation '{name}' was out of bounds"))?
            .to_string();

        spans.push(GenerationSpan {
            name,
            is_preview,
            text,
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
    fn parses_a_single_stable_generation() -> Result<()> {
        let source = wrap(
            "    version y2026_1 {\n        drive {\n            /// docs with a { brace } too\n            struct Target { x: f32 }\n            topic target: command Target;\n        }\n    }",
        );
        let spans = parse_generation_spans(&source)?;
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "y2026_1");
        assert!(!spans[0].is_preview);
        assert!(spans[0].text.starts_with("version y2026_1"));
        assert!(spans[0].text.trim_end().ends_with('}'));
        Ok(())
    }

    #[test]
    fn parses_a_stable_and_a_preview_generation() -> Result<()> {
        let source = wrap(
            "    version y2026_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version y2026_2 {\n        drive { struct Target { x: f32, y: f32 } topic target: command Target; }\n    }",
        );
        let spans = parse_generation_spans(&source)?;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].name, "y2026_1");
        assert!(!spans[0].is_preview);
        assert_eq!(spans[1].name, "y2026_2");
        assert!(spans[1].is_preview);
        assert!(spans[1].text.starts_with("preview version y2026_2"));
        Ok(())
    }

    #[test]
    fn unchanged_frozen_generation_passes() -> Result<()> {
        let baseline = wrap(
            "    version y2026_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = baseline.clone();
        let violations = frozen_generation_violations("phoxal-api-v0.1.0", &baseline, &current)?;
        assert!(violations.is_empty(), "{violations:?}");
        Ok(())
    }

    #[test]
    fn editing_a_frozen_generation_fails() -> Result<()> {
        let baseline = wrap(
            "    version y2026_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = wrap(
            "    version y2026_1 {\n        drive { struct Target { x: f64 } topic target: command Target; }\n    }",
        );
        let violations = frozen_generation_violations("phoxal-api-v0.1.0", &baseline, &current)?;
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("y2026_1"));
        assert!(violations[0].contains("must not change"));
        Ok(())
    }

    #[test]
    fn demoting_a_frozen_generation_to_preview_fails() -> Result<()> {
        let baseline = wrap(
            "    version y2026_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = wrap(
            "    preview version y2026_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let violations = frozen_generation_violations("phoxal-api-v0.1.0", &baseline, &current)?;
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("preview"));
        Ok(())
    }

    #[test]
    fn adding_a_new_generation_does_not_fail() -> Result<()> {
        let baseline = wrap(
            "    version y2026_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = wrap(
            "    version y2026_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    preview version y2026_2 {\n        drive { struct Target { x: f32, y: f32 } topic target: command Target; }\n    }",
        );
        let violations = frozen_generation_violations("phoxal-api-v0.1.0", &baseline, &current)?;
        assert!(violations.is_empty(), "{violations:?}");
        Ok(())
    }

    #[test]
    fn promoting_a_generation_that_was_already_preview_at_baseline_does_not_fail() -> Result<()> {
        let baseline = wrap(
            "    preview version y2026_2 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let current = wrap(
            "    version y2026_2 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let violations = frozen_generation_violations("phoxal-api-v0.1.0", &baseline, &current)?;
        assert!(violations.is_empty(), "{violations:?}");
        Ok(())
    }

    #[test]
    fn removing_a_frozen_generation_fails() -> Result<()> {
        let baseline = wrap(
            "    version y2026_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }\n    version y2026_2 {\n        drive { struct Other { x: f32 } topic other: command Other; }\n    }",
        );
        let current = wrap(
            "    version y2026_1 {\n        drive { struct Target { x: f32 } topic target: command Target; }\n    }",
        );
        let violations = frozen_generation_violations("phoxal-api-v0.1.0", &baseline, &current)?;
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("y2026_2"));
        assert!(violations[0].contains("missing"));
        Ok(())
    }

    #[test]
    fn no_matching_tag_returns_none() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir.path())
            .status()
            .context("git init")?;
        assert!(status.success());
        let result = latest_matching_tag(dir.path(), "phoxal-api-v*")?;
        assert!(result.is_none());
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
