//! Strict-YAML pre-checks for `robot.yaml`.
//!
//! `serde_yaml` 0.9 already rejects duplicate mapping keys natively, and it
//! already treats YAML 1.1-only booleans (`yes`/`no`/`on`/`off`) as plain
//! strings rather than coercing them - so a field typed `bool` already only
//! accepts `true`/`false`. What `serde_yaml` does *not* reject on its own:
//! anchors/aliases (`&name`, `*name`), merge keys (`<<`), explicit tags
//! (`!!str`, `!Custom`), and multi-document streams. This module rejects
//! those before the manifest reaches `serde_yaml::from_str`.
//!
//! The scan is a lightweight character-class check, not a full YAML
//! tokenizer: it tracks single/double-quoted scalar spans (which may
//! legitimately contain `&`, `*`, `#`) and `#` comments, and flags the
//! anchor/alias/tag/merge-key markers only when they appear outside of those
//! spans, in a position where YAML would treat them as syntax (start of a
//! token, after `:`, after `-`, or after whitespace).

use anyhow::{Result, bail};

/// Rejects anchors, aliases, merge keys, explicit tags, and multi-document
/// streams. Returns `Ok(())` when the text is clear of all of them.
pub fn check(text: &str) -> Result<()> {
    check_single_document(text)?;
    check_no_reserved_markers(text)?;
    Ok(())
}

fn check_single_document(text: &str) -> Result<()> {
    let document_count = serde_yaml::Deserializer::from_str(text).count();
    if document_count > 1 {
        bail!(
            "robot.yaml must be a single YAML document; found {document_count} \
             (multi-document `---` streams are rejected by strict parsing)"
        );
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
        None | Some(':') | Some('-') | Some('[') | Some(',')
    )
}

fn check_no_reserved_markers(text: &str) -> Result<()> {
    let mut quote = Quote::None;
    let mut in_comment = false;
    let mut prev_non_space: Option<char> = None;
    let mut line_no: usize = 1;
    let mut col_no: usize = 0;

    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        col_no += 1;
        if ch == '\n' {
            line_no += 1;
            col_no = 0;
            in_comment = false;
            prev_non_space = None;
            continue;
        }
        if in_comment {
            continue;
        }
        match quote {
            Quote::Single => {
                if ch == '\'' {
                    if chars.peek() == Some(&'\'') {
                        // Escaped '' inside a single-quoted scalar.
                        chars.next();
                        col_no += 1;
                    } else {
                        quote = Quote::None;
                    }
                }
                prev_non_space = Some(ch);
                continue;
            }
            Quote::Double => {
                if ch == '\\' {
                    // Skip the escaped character.
                    chars.next();
                    col_no += 1;
                } else if ch == '"' {
                    quote = Quote::None;
                }
                prev_non_space = Some(ch);
                continue;
            }
            Quote::None => {}
        }

        match ch {
            '\'' => {
                quote = Quote::Single;
                prev_non_space = Some(ch);
            }
            '"' => {
                quote = Quote::Double;
                prev_non_space = Some(ch);
            }
            '#' if matches!(prev_non_space, None | Some(' ') | Some('\t')) => {
                in_comment = true;
            }
            '&' if starts_token(prev_non_space) => {
                bail!(
                    "robot.yaml:{line_no}:{col_no}: YAML anchors ('&name') are rejected by \
                     strict parsing"
                );
            }
            '*' if starts_token(prev_non_space) => {
                bail!(
                    "robot.yaml:{line_no}:{col_no}: YAML aliases ('*name') are rejected by \
                     strict parsing"
                );
            }
            '!' if starts_token(prev_non_space) => {
                bail!(
                    "robot.yaml:{line_no}:{col_no}: explicit YAML tags ('!...') are rejected by \
                     strict parsing"
                );
            }
            '<' if starts_token(prev_non_space) && chars.peek() == Some(&'<') => {
                bail!(
                    "robot.yaml:{line_no}:{col_no}: YAML merge keys ('<<') are rejected by \
                     strict parsing"
                );
            }
            ' ' | '\t' => {
                // Whitespace does not change prev_non_space.
                continue;
            }
            _ => {
                prev_non_space = Some(ch);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn accepts_plain_manifest_text() {
        check("schema: v0\nrobot:\n  id: rover\n  namespace: dev\n").expect("plain text is fine");
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
    fn rejects_anchor() {
        let error = check("a: &x 1\nb: 2\n").expect_err("anchor should be rejected");
        assert!(format!("{error:#}").contains("anchor"), "got: {error:#}");
    }

    #[test]
    fn rejects_alias() {
        let error = check("a: &x 1\nb: *x\n").expect_err("alias should be rejected");
        assert!(format!("{error:#}").contains("anchor"), "got: {error:#}");
    }

    #[test]
    fn rejects_merge_key() {
        let error = check("base: &b\n  x: 1\nfoo:\n  <<: *b\n  y: 2\n")
            .expect_err("merge key should be rejected");
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
