//! The rule that a committed comment names no issue, pull request or decision
//! record.
//!
//! A comment that cites `organization#<number>` or `(D<number>)` records the
//! argument that produced the code instead of the invariant the code holds.
//! The citation is unreachable from a checkout, it rots as soon as the tracker
//! is reorganized, and it invites the next reader to go looking for context
//! that the comment should have stated outright. The fix is always the same:
//! write the invariant, drop the number.
//!
//! The one defensible exception is an *upstream* tracking issue that a
//! workaround depends on, because it names the exact condition under which the
//! workaround can be deleted. That case is allowed only in its fully qualified
//! `<owner>/<repo>#<number>` form, and never for this project's own
//! organization, whose tracker is history rather than an upstream condition.
//! A bare `#<number>` is always rejected: it does not even say which tracker it
//! means.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The owner whose trackers are this project's own. A reference into them is
/// history, so it is rejected even when it is fully qualified.
const PROJECT_OWNER: &str = "phoxal";

/// Directories a source scan never descends into: build output and version
/// control state, neither of which is committed source.
const SKIPPED_DIRS: [&str; 2] = ["target", ".git"];

/// Comment syntax a scanned file is written in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommentSyntax {
    /// `//` to end of line and nested `/* */`, with string, raw-string and
    /// character literals skipped so a reference inside a literal is code, not
    /// a comment.
    Rust,
    /// `#` to end of line, as TOML, YAML and shell all spell it.
    Hash,
}

impl CommentSyntax {
    /// The syntax of a source file, or `None` for a file this rule does not
    /// scan.
    ///
    /// Markdown is deliberately absent. Prose is where a link to a tracker
    /// belongs, and the generated changelog is nothing but such links.
    pub fn for_path(path: &Path) -> Option<Self> {
        match path.extension().and_then(OsStr::to_str)? {
            "rs" => Some(Self::Rust),
            "toml" | "yml" | "yaml" => Some(Self::Hash),
            _ => None,
        }
    }

    /// The comment text in `source`, one entry per line so that every entry
    /// carries the 1-based line the text is actually on.
    fn comments(self, source: &str) -> Vec<(usize, &str)> {
        match self {
            Self::Rust => rust_comments(source),
            Self::Hash => hash_comments(source),
        }
    }
}

/// One issue, pull-request or decision reference found in a comment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentReference {
    /// The file the reference is in, as the scan named it.
    pub path: PathBuf,
    /// The 1-based line the reference is on.
    pub line: usize,
    /// The reference exactly as the comment writes it, qualifier included.
    pub text: String,
}

impl CommentReference {
    /// Every rejected reference in one file's comments.
    pub fn scan_source(path: &Path, source: &str, syntax: CommentSyntax) -> Vec<Self> {
        syntax
            .comments(source)
            .into_iter()
            .flat_map(|(line, comment)| {
                rejected_references(comment)
                    .into_iter()
                    .map(move |text| Self {
                        path: path.to_path_buf(),
                        line,
                        text: text.to_owned(),
                    })
            })
            .collect()
    }

    /// Every rejected reference in the comments of every scannable file under
    /// `root`, reported with paths relative to `root`.
    pub fn scan_tree(root: &Path) -> Result<Vec<Self>> {
        let mut found = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let entries = fs::read_dir(&directory)
                .with_context(|| format!("failed to read {}", directory.display()))?;
            for entry in entries {
                let entry = entry.with_context(|| {
                    format!("failed to read an entry in {}", directory.display())
                })?;
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .with_context(|| format!("failed to stat {}", path.display()))?;
                if file_type.is_symlink() {
                    continue;
                }
                if file_type.is_dir() {
                    let name = entry.file_name();
                    if !SKIPPED_DIRS
                        .iter()
                        .any(|skipped| OsStr::new(skipped) == name)
                    {
                        pending.push(path);
                    }
                    continue;
                }
                let Some(syntax) = CommentSyntax::for_path(&path) else {
                    continue;
                };
                let source = fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let relative = path.strip_prefix(root).unwrap_or(&path);
                found.extend(Self::scan_source(relative, &source, syntax));
            }
        }
        found.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.line.cmp(&right.line))
        });
        Ok(found)
    }
}

impl fmt::Display for CommentReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}: {}",
            self.path.display(),
            self.line,
            self.text
        )
    }
}

/// The references in one comment fragment that this rule rejects.
fn rejected_references(comment: &str) -> Vec<&str> {
    let bytes = comment.as_bytes();
    let mut rejected = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(end) = decision_id_end(bytes, index) {
            rejected.push(&comment[index..end]);
            index = end;
            continue;
        }
        if bytes[index] == b'#'
            && let Some(end) = number_end(bytes, index + 1)
        {
            let start = qualifier_start(bytes, index);
            if !is_allowed_upstream_owner(&comment[start..index]) {
                rejected.push(&comment[start..end]);
            }
            index = end;
            continue;
        }
        index += 1;
    }
    rejected
}

/// The end of a `(D<number>)`-style decision record id starting at `index`.
fn decision_id_end(bytes: &[u8], index: usize) -> Option<usize> {
    if bytes.get(index) != Some(&b'(') || bytes.get(index + 1) != Some(&b'D') {
        return None;
    }
    let end = number_end(bytes, index + 2)?;
    (bytes.get(end) == Some(&b')')).then_some(end + 1)
}

/// The end of the run of digits at `start`, or `None` when there is no such
/// run or when it runs into a word. `#00ff00` is a color, not a reference.
fn number_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut end = start;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    let terminated = !bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
    (end > start && terminated).then_some(end)
}

/// The start of the tracker qualifier immediately before the `#` at `index`.
fn qualifier_start(bytes: &[u8], index: usize) -> usize {
    let mut start = index;
    while start > 0 && is_qualifier_byte(bytes[start - 1]) {
        start -= 1;
    }
    start
}

fn is_qualifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/')
}

/// Whether a qualifier names an upstream repository whose tracker a workaround
/// may legitimately cite: exactly `<owner>/<repo>`, and not this project's own
/// organization.
fn is_allowed_upstream_owner(qualifier: &str) -> bool {
    let Some((owner, repository)) = qualifier.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && !repository.is_empty()
        && !repository.contains('/')
        && owner != PROJECT_OWNER
}

/// One entry per line of comment text, so a reference in a block comment
/// reports its own line rather than the comment's first.
fn rust_comments(source: &str) -> Vec<(usize, &str)> {
    let bytes = source.as_bytes();
    let mut comments = Vec::new();
    let mut line = 1;
    let mut index = 0;
    while index < bytes.len() {
        if let Some((end, lines)) = literal_end(bytes, index) {
            index = end;
            line += lines;
            continue;
        }
        match (bytes[index], bytes.get(index + 1)) {
            (b'\n', _) => {
                line += 1;
                index += 1;
            }
            (b'/', Some(b'/')) => {
                let start = index;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                comments.push((line, &source[start..index]));
            }
            (b'/', Some(b'*')) => {
                let mut depth = 0;
                let mut start = index;
                while index < bytes.len() {
                    match (bytes[index], bytes.get(index + 1)) {
                        (b'/', Some(b'*')) => {
                            depth += 1;
                            index += 2;
                        }
                        (b'*', Some(b'/')) => {
                            depth -= 1;
                            index += 2;
                            if depth == 0 {
                                break;
                            }
                        }
                        (b'\n', _) => {
                            comments.push((line, &source[start..index]));
                            line += 1;
                            index += 1;
                            start = index;
                        }
                        _ => index += 1,
                    }
                }
                comments.push((line, &source[start..index.min(bytes.len())]));
            }
            _ => index += 1,
        }
    }
    comments
}

/// The end of the string, raw-string or character literal starting at `index`,
/// with the number of newlines it spans, or `None` when no literal starts
/// there.
fn literal_end(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    let mut cursor = index;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    let raw = bytes.get(cursor) == Some(&b'r');
    if raw {
        cursor += 1;
    }
    let mut hashes = 0;
    if raw {
        while bytes.get(cursor) == Some(&b'#') {
            hashes += 1;
            cursor += 1;
        }
    }
    match bytes.get(cursor) {
        Some(&b'"') => Some(string_literal_end(bytes, cursor + 1, raw.then_some(hashes))),
        Some(&b'\'') if !raw && cursor == index => character_literal_end(bytes, cursor),
        _ => None,
    }
}

/// Scans to the closing quote of a string that has already been opened. An
/// unterminated literal ends the file, which is what the compiler would say
/// about it too.
fn string_literal_end(bytes: &[u8], mut index: usize, raw_hashes: Option<usize>) -> (usize, usize) {
    let mut lines = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                lines += 1;
                index += 1;
            }
            // An escape hides the byte behind it, including the newline of a
            // line continuation, which still moves the line counter on.
            b'\\' if raw_hashes.is_none() => {
                if bytes.get(index + 1) == Some(&b'\n') {
                    lines += 1;
                }
                index += 2;
            }
            b'"' => {
                let hashes = raw_hashes.unwrap_or(0);
                let closed = (1..=hashes).all(|offset| bytes.get(index + offset) == Some(&b'#'));
                if closed {
                    return (index + 1 + hashes, lines);
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    (bytes.len(), lines)
}

/// A `'` opens a character literal only when one character and a closing quote
/// follow it; otherwise it is a lifetime, and consuming code after a lifetime
/// could hide a comment.
fn character_literal_end(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    let mut cursor = index + 1;
    if bytes.get(cursor) == Some(&b'\\') {
        // The longest escape is `\u{10FFFF}`, so the quote is close behind.
        let limit = (index + 12).min(bytes.len());
        cursor += 2;
        while cursor < limit {
            match bytes[cursor] {
                b'\'' => return Some((cursor + 1, 0)),
                b'\n' => return None,
                _ => cursor += 1,
            }
        }
        return None;
    }
    cursor += 1;
    while bytes.get(cursor).is_some_and(is_utf8_continuation) {
        cursor += 1;
    }
    (bytes.get(cursor) == Some(&b'\'')).then_some((cursor + 1, 0))
}

fn is_utf8_continuation(byte: &u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

fn hash_comments(source: &str) -> Vec<(usize, &str)> {
    source
        .lines()
        .enumerate()
        .filter_map(|(index, text)| {
            hash_comment_start(text).map(|start| (index + 1, &text[start..]))
        })
        .collect()
}

/// The byte index of the `#` that opens a comment on this line, if any. A `#`
/// inside a quoted scalar is data, and (as YAML requires) a comment's `#` must
/// start the line or follow whitespace.
fn hash_comment_start(line: &str) -> Option<usize> {
    let mut quote = None;
    let mut after_space = true;
    for (index, character) in line.char_indices() {
        match quote {
            Some(open) => {
                if character == open {
                    quote = None;
                }
            }
            None => match character {
                '"' | '\'' => quote = Some(character),
                '#' if after_space => return Some(index),
                _ => {}
            },
        }
        after_space = character.is_whitespace();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_root;

    fn scan(source: &str, syntax: CommentSyntax) -> Vec<String> {
        CommentReference::scan_source(Path::new("probe"), source, syntax)
            .into_iter()
            .map(|reference| format!("{}:{}", reference.line, reference.text))
            .collect()
    }

    #[test]
    fn a_bare_number_names_no_tracker_and_is_rejected() {
        assert_eq!(
            scan("// see #951 for the rationale\n", CommentSyntax::Rust),
            ["1:#951"]
        );
        assert_eq!(
            scan("// plan #00 said otherwise\n", CommentSyntax::Rust),
            ["1:#00"]
        );
        assert_eq!(
            scan("// organization#951, Decision 2\n", CommentSyntax::Rust),
            ["1:organization#951"]
        );
    }

    /// The single defensible citation: an upstream tracker that names the
    /// condition under which a workaround can go. This project's own
    /// organization is not upstream, however it is spelled.
    #[test]
    fn only_fully_qualified_upstream_references_are_allowed() {
        assert!(
            scan(
                "/// but is unstable: rust-lang/rust#93798\n",
                CommentSyntax::Rust
            )
            .is_empty()
        );
        assert!(
            scan(
                "# after zenoh ships the fix (eclipse-zenoh/zenoh#2499)\n",
                CommentSyntax::Hash
            )
            .is_empty()
        );
        assert_eq!(
            scan("// see phoxal/organization#951\n", CommentSyntax::Rust),
            ["1:phoxal/organization#951"]
        );
        assert_eq!(
            scan("// see phoxal/framework#395\n", CommentSyntax::Rust),
            ["1:phoxal/framework#395"]
        );
    }

    #[test]
    fn decision_record_ids_are_rejected() {
        assert_eq!(
            scan(
                "// the fail-closed rule (D34) applies here\n",
                CommentSyntax::Rust
            ),
            ["1:(D34)"]
        );
        assert!(scan("// (Debug) is not an id\n", CommentSyntax::Rust).is_empty());
    }

    /// A number that runs into a word is not a reference: `#00ff00` is a color
    /// and `#0withsuffix` is not a tracker at all.
    #[test]
    fn numbers_that_run_into_words_are_not_references() {
        assert!(scan("// background #00ff00 everywhere\n", CommentSyntax::Rust).is_empty());
        assert!(scan("// heading # 1 is prose\n", CommentSyntax::Rust).is_empty());
        assert!(scan("// an attribute #[derive(Debug)]\n", CommentSyntax::Rust).is_empty());
    }

    #[test]
    fn only_comments_are_scanned_in_rust_sources() {
        let source = concat!(
            "let issue = \"organization#951\";\n",
            "let raw = r#\"see #951\"#;\n",
            "let quote = '\"';\n",
            "// but this one counts: #952\n",
        );
        assert_eq!(scan(source, CommentSyntax::Rust), ["4:#952"]);
    }

    #[test]
    fn a_block_comment_reports_the_line_the_reference_is_on() {
        let source = concat!(
            "/* opening line\n",
            " * middle line mentioning #951\n",
            " * and /* a nested comment with #952 */ still inside\n",
            " */\n",
            "fn code() {}\n",
        );
        assert_eq!(scan(source, CommentSyntax::Rust), ["2:#951", "3:#952"]);
    }

    #[test]
    fn a_quoted_hash_is_data_and_not_a_comment() {
        let source = concat!(
            "value = \"tracked as #951\"\n",
            "# but this comment is not (organization#952)\n",
            "pattern = 'x#953'\n",
        );
        assert_eq!(scan(source, CommentSyntax::Hash), ["2:organization#952"]);
    }

    #[test]
    fn markdown_and_lockfiles_are_not_scanned() {
        assert_eq!(
            CommentSyntax::for_path(Path::new("phoxal/src/lib.rs")),
            Some(CommentSyntax::Rust)
        );
        assert_eq!(
            CommentSyntax::for_path(Path::new(".github/workflows/ci.yml")),
            Some(CommentSyntax::Hash)
        );
        assert_eq!(CommentSyntax::for_path(Path::new("CHANGELOG.md")), None);
        assert_eq!(CommentSyntax::for_path(Path::new("Cargo.lock")), None);
    }

    /// The rule holds over the crate that defines it, whatever the rest of the
    /// tree still carries. This is also the only test that walks a real
    /// directory tree rather than a synthetic source.
    #[test]
    fn this_crate_carries_no_issue_or_decision_references() -> Result<()> {
        let found = CommentReference::scan_tree(&workspace_root()?.join("workspace-policy"))?;
        assert!(found.is_empty(), "{}", report(&found));
        Ok(())
    }

    /// The workspace-wide gate.
    ///
    /// Historical rationale belongs in Git and in the issue tracker, where it
    /// stays accurate; a decision id pasted into a comment is a pointer that
    /// rots silently and that a reader cannot resolve from the source alone.
    /// Comments here carry the invariant that holds today, and this test is
    /// what keeps the previous state from creeping back in one comment at a
    /// time. A genuinely load-bearing upstream reference is still allowed in
    /// its fully qualified `<owner>/<repo>#<number>` form, because that names
    /// the condition under which a workaround can be removed.
    #[test]
    fn comments_across_the_workspace_carry_no_issue_or_decision_references() -> Result<()> {
        let found = CommentReference::scan_tree(workspace_root()?)?;
        assert!(found.is_empty(), "{}", report(&found));
        Ok(())
    }

    fn report(found: &[CommentReference]) -> String {
        std::iter::once("comments must state the invariant, not cite a tracker:".to_owned())
            .chain(found.iter().map(CommentReference::to_string))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
