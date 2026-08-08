//! The strict YAML subset authored documents are read in.
//!
//! `serde_yaml` 0.9 already rejects duplicate mapping keys natively, and it
//! already treats YAML 1.1-only booleans (`yes`/`no`/`on`/`off`) as plain
//! strings rather than coercing them - so a field typed `bool` already only
//! accepts `true`/`false`. What `serde_yaml` does *not* reject on its own:
//! anchors/aliases (`&name`, `*name`), merge keys (`<<`), explicit tags
//! (`!!str`, `!Custom`), and multi-document streams. This module rejects
//! those before the document reaches `serde_yaml::from_str`.
//!
//! The scan is a lightweight character-class check, not a full YAML
//! tokenizer: it tracks single/double-quoted scalar spans (which may
//! legitimately contain `&`, `*`, `#`), `#` comments, and literal/folded block
//! scalar bodies, and flags the anchor/alias/tag/merge-key markers only when
//! they appear outside of those spans, in a position where YAML would treat them
//! as syntax (start of a token or after a value/sequence/flow introducer).

use std::fmt;

/// A YAML construct authored documents are read without.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservedMarker {
    Anchor,
    Alias,
    Tag,
    MergeKey,
}

impl fmt::Display for ReservedMarker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Anchor => "YAML anchors ('&name')",
            Self::Alias => "YAML aliases ('*name')",
            Self::Tag => "explicit YAML tags ('!...')",
            Self::MergeKey => "YAML merge keys ('<<')",
        })
    }
}

/// Text that is YAML but outside the subset authored documents are read in.
///
/// The offending position is carried rather than the file name: which document
/// this came from is the caller's to say, and a document parsed from memory has
/// no file name at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StrictYamlError {
    #[error("line {line}, column {column}: {marker} are rejected by strict parsing")]
    Reserved {
        marker: ReservedMarker,
        line: usize,
        column: usize,
    },
    #[error(
        "must be a single YAML document; found {count} (multi-document `---` streams are \
         rejected by strict parsing)"
    )]
    MultipleDocuments { count: usize },
}

type Result<T> = std::result::Result<T, StrictYamlError>;

/// Rejects anchors, aliases, merge keys, explicit tags, and multi-document
/// streams.
///
/// # Errors
///
/// Returns the first [`StrictYamlError`] the text triggers.
pub fn check(text: &str) -> Result<()> {
    check_no_reserved_markers(text)?;
    check_single_document(text)?;
    Ok(())
}

fn check_single_document(text: &str) -> Result<()> {
    let count = serde_yaml::Deserializer::from_str(text).count();
    if count > 1 {
        return Err(StrictYamlError::MultipleDocuments { count });
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

/// Whether a marker char (`&`, `*`, `!`) at `pos` starts a new YAML token,
/// i.e. it is at the start of the (trimmed) line or immediately follows one
/// of the characters YAML treats as a value/sequence introducer.
fn starts_token(prev_non_space: Option<char>) -> bool {
    matches!(
        prev_non_space,
        None | Some(':') | Some('-') | Some('[') | Some('{') | Some(',') | Some('?')
    )
}

fn check_no_reserved_markers(text: &str) -> Result<()> {
    let mut quote = Quote::None;
    let mut block_scalar_parent_indent: Option<usize> = None;

    for (line_index, raw_line) in text.split_inclusive('\n').enumerate() {
        let line_no = line_index + 1;
        let line = raw_line.trim_end_matches(['\r', '\n']);

        if let Some(parent_indent) = block_scalar_parent_indent {
            if line.trim().is_empty() {
                continue;
            }
            if line_indent(line) > parent_indent {
                continue;
            }
            block_scalar_parent_indent = None;
        }

        if let Some(parent_indent) = check_line_no_reserved_markers(line, line_no, &mut quote)? {
            block_scalar_parent_indent = Some(parent_indent);
        }
    }

    Ok(())
}

/// Scan one line. `Ok(Some(indent))` means the line opened a block scalar whose
/// body starts at that parent indent, `Ok(None)` means it did not, and `Err`
/// means the line carries a marker the strict subset rejects - three distinct
/// outcomes, not two spellings of "nothing found".
fn check_line_no_reserved_markers(
    line: &str,
    line_no: usize,
    quote: &mut Quote,
) -> Result<Option<usize>> {
    let mut in_comment = false;
    let mut prev_char: Option<char> = None;
    let mut prev_non_space: Option<char> = None;
    let mut chars = line.chars().enumerate().peekable();
    while let Some((index, ch)) = chars.next() {
        let col_no = index + 1;
        if in_comment {
            continue;
        }
        match *quote {
            Quote::Single => {
                if ch == '\'' {
                    if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                        // Escaped '' inside a single-quoted scalar.
                        chars.next();
                    } else {
                        *quote = Quote::None;
                    }
                }
                prev_char = Some(ch);
                prev_non_space = Some(ch);
                continue;
            }
            Quote::Double => {
                if ch == '\\' {
                    // Skip the escaped character.
                    chars.next();
                } else if ch == '"' {
                    *quote = Quote::None;
                }
                prev_char = Some(ch);
                prev_non_space = Some(ch);
                continue;
            }
            Quote::None => {}
        }

        match ch {
            '\'' => {
                *quote = Quote::Single;
                prev_char = Some(ch);
                prev_non_space = Some(ch);
            }
            '"' => {
                *quote = Quote::Double;
                prev_char = Some(ch);
                prev_non_space = Some(ch);
            }
            '#' if matches!(prev_char, None | Some(' ') | Some('\t')) => {
                in_comment = true;
            }
            '|' | '>' if starts_token(prev_non_space) && is_block_scalar_header_tail(&chars) => {
                return Ok(Some(line_indent(line)));
            }
            '&' if starts_token(prev_non_space) => {
                return Err(StrictYamlError::Reserved {
                    marker: ReservedMarker::Anchor,
                    line: line_no,
                    column: col_no,
                });
            }
            '*' if starts_token(prev_non_space) => {
                return Err(StrictYamlError::Reserved {
                    marker: ReservedMarker::Alias,
                    line: line_no,
                    column: col_no,
                });
            }
            '!' if starts_token(prev_non_space) => {
                return Err(StrictYamlError::Reserved {
                    marker: ReservedMarker::Tag,
                    line: line_no,
                    column: col_no,
                });
            }
            '<' if starts_token(prev_non_space)
                && chars.peek().is_some_and(|(_, next)| *next == '<') =>
            {
                return Err(StrictYamlError::Reserved {
                    marker: ReservedMarker::MergeKey,
                    line: line_no,
                    column: col_no,
                });
            }
            ' ' | '\t' => {
                // Whitespace does not change prev_non_space.
                prev_char = Some(ch);
                continue;
            }
            _ => {
                prev_char = Some(ch);
                prev_non_space = Some(ch);
            }
        }
    }

    Ok(None)
}

fn line_indent(line: &str) -> usize {
    line.chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .count()
}

fn is_block_scalar_header_tail<I>(chars: &std::iter::Peekable<I>) -> bool
where
    I: Iterator<Item = (usize, char)> + Clone,
{
    let mut seen_chomping = false;
    let mut seen_indent = false;
    let mut tail = chars.clone();

    while let Some((_, ch)) = tail.peek().copied() {
        match ch {
            '+' | '-' if !seen_chomping => {
                seen_chomping = true;
                tail.next();
            }
            '0'..='9' if !seen_indent => {
                seen_indent = true;
                tail.next();
            }
            ' ' | '\t' => break,
            '#' => break,
            _ => return false,
        }
    }

    for (_, ch) in tail {
        match ch {
            ' ' | '\t' => {}
            '#' => return true,
            _ => return false,
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn accepts_plain_manifest_text() {
        check("schema: phoxal/robot/v0\nrobot:\n  id: rover\n  namespace: dev\n")
            .expect("plain text is fine");
    }

    #[test]
    fn accepts_quoted_strings_containing_marker_characters() {
        check("a: \"AT&T corp\"\nb: 'star * here'\nc: \"tag !like this\"\n")
            .expect("marker characters inside quotes are not markers");
    }

    #[test]
    fn accepts_comments_containing_marker_characters() {
        check("a: 1 # trailing & * ! << comment\nb: 2\n").expect("comment text is not scanned");
    }

    #[test]
    fn accepts_block_scalars_containing_marker_characters() {
        check(
            "description: |\n  literal & anchor-looking text\n  alias *x text\n  tag !Thing text\n  merge << text\nnext: 1\n",
        )
        .expect("block scalar contents are plain text, not YAML markers");
    }

    #[test]
    fn rejects_anchor() {
        let error = check("a: &x 1\nb: 2\n").expect_err("anchor should be rejected");
        assert!(format!("{error:#}").contains("anchor"), "got: {error:#}");
    }

    #[test]
    fn rejects_alias() {
        let error = check("a: *x\n").expect_err("alias should be rejected");
        assert!(format!("{error:#}").contains("alias"), "got: {error:#}");
    }

    #[test]
    fn rejects_merge_key() {
        let error =
            check("foo:\n  <<: {x: 1}\n  y: 2\n").expect_err("merge key should be rejected");
        assert!(format!("{error:#}").contains("merge"), "got: {error:#}");
    }

    #[test]
    fn rejects_flow_style_reserved_markers() {
        let error = check("foo: {<<: {x: 1}}\n").expect_err("flow merge key should be rejected");
        assert!(format!("{error:#}").contains("merge"), "got: {error:#}");

        let error = check("foo: {&name x: 1}\n").expect_err("flow anchor should be rejected");
        assert!(format!("{error:#}").contains("anchor"), "got: {error:#}");
    }

    #[test]
    fn rejects_explicit_tag() {
        let error = check("a: !!str 123\n").expect_err("explicit tag should be rejected");
        assert!(format!("{error:#}").contains("tag"), "got: {error:#}");
    }

    #[test]
    fn rejects_multi_document_stream() {
        let error =
            check("a: 1\n---\nb: 2\n").expect_err("multi-document stream should be rejected");
        assert!(
            format!("{error:#}").contains("single YAML document"),
            "got: {error:#}"
        );
    }
}
