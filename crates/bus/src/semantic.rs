//! Compile-time guard for the semantic contract on author-facing bus handles.

/// The sections every author-facing bus handle must document, in this order.
pub(crate) const REQUIRED_SECTIONS: [&str; 4] = [
    "Meaning:",
    "Timestamp:",
    "Buffering and admission:",
    "Observable overload or loss:",
];

/// Whether `documentation` contains every required semantic section in order.
///
/// Kept `const` so the declaration carrying the documentation can also prove
/// the contract at compile time. This makes the attached rustdoc the single
/// source of truth; tests never need to inspect Rust source text.
pub(crate) const fn has_required_sections(documentation: &str) -> bool {
    let bytes = documentation.as_bytes();
    let first = REQUIRED_SECTIONS[0].as_bytes();
    if !matches_at(bytes, first, 0) {
        return false;
    }

    let mut cursor = first.len();
    let mut section = 1;

    while section < REQUIRED_SECTIONS.len() {
        let heading = REQUIRED_SECTIONS[section].as_bytes();
        let Some(position) = find_paragraph_heading(bytes, heading, cursor) else {
            return false;
        };
        if !has_non_whitespace(bytes, cursor, position - 2) {
            return false;
        }
        cursor = position + heading.len();
        section += 1;
    }

    has_non_whitespace(bytes, cursor, bytes.len())
}

const fn find_paragraph_heading(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    let mut position = start;
    while position + needle.len() <= haystack.len() {
        if position >= 2
            && haystack[position - 2] == b'\n'
            && haystack[position - 1] == b'\n'
            && matches_at(haystack, needle, position)
        {
            return Some(position);
        }
        position += 1;
    }
    None
}

const fn matches_at(haystack: &[u8], needle: &[u8], position: usize) -> bool {
    let mut offset = 0;
    while offset < needle.len() {
        if haystack[position + offset] != needle[offset] {
            return false;
        }
        offset += 1;
    }
    true
}

const fn has_non_whitespace(bytes: &[u8], start: usize, end: usize) -> bool {
    let mut position = start;
    while position < end {
        if !bytes[position].is_ascii_whitespace() {
            return true;
        }
        position += 1;
    }
    false
}

macro_rules! author_semantic_docs {
    ($documentation:literal; $item:item) => {
        #[doc = $documentation]
        $item

        const _: () = assert!(
            $crate::semantic::has_required_sections($documentation),
            "author-facing bus documentation is missing a required semantic section"
        );
    };
}

pub(crate) use author_semantic_docs;

#[cfg(test)]
mod tests {
    use super::has_required_sections;

    const COMPLETE: &str = "Meaning: state.\n\nTimestamp: logical.\n\nBuffering and admission: bounded.\n\nObservable overload or loss: typed.";

    #[test]
    fn accepts_complete_ordered_semantic_documentation() {
        assert!(has_required_sections(COMPLETE));
    }

    #[test]
    fn rejects_missing_or_reordered_semantic_sections() {
        assert!(!has_required_sections(
            "Meaning: state.\n\nTimestamp: logical.\n\nObservable overload or loss: typed."
        ));
        assert!(!has_required_sections(
            "Timestamp: logical.\n\nMeaning: state.\n\nBuffering and admission: bounded.\n\nObservable overload or loss: typed."
        ));
        assert!(!has_required_sections(
            "Meaning:\n\nTimestamp: logical.\n\nBuffering and admission: bounded.\n\nObservable overload or loss: typed."
        ));
    }
}
