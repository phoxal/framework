//! Where a Cargo feature may be named in the framework library's source.
//!
//! A consumer profile decides which modules are `pub`. It never forks a code
//! path inside a module, because the moment it does the one library starts
//! growing the old package topology back as scattered `#[cfg]`s: two builds of
//! one crate that behave differently, with no name for either of them.
//!
//! So a feature is named in exactly two places - the optional dependencies in
//! `phoxal/Cargo.toml`, and the module declarations in `phoxal/src/lib.rs` - and
//! this rule enforces the second half. The first half needs no rule: a
//! `[features]` table is the only place Cargo lets a feature be declared.
//!
//! Whole module trees whose entire content belongs to one profile are gated as a
//! unit at their `lib.rs` declaration, which is why one file is enough to say
//! everything a profile changes.

use std::fs;

use anyhow::{Context, Result};

use super::tracked_source;
use super::{Subject, Violation};

/// The tree this rule reads.
const LIBRARY_SOURCE: &str = "phoxal/src/";

/// The one file in it that may name a feature.
const PROFILE_DECLARATIONS: &str = "phoxal/src/lib.rs";

/// The two conditional-compilation forms, matched with their opening
/// parenthesis so the predicate can be read out of them.
const GATES: [&str; 2] = ["cfg(", "cfg_attr("];

/// What makes one of them a *profile* gate rather than an ordinary one.
///
/// `#[cfg(test)]` and `#[cfg(unix)]` are not what this rule is about: they say
/// which build of the same product is being compiled, not which of several
/// products a consumer selected.
const PROFILE_PREDICATE: &str = "feature";

/// The framework library compiles the same code under every profile.
pub(super) fn feature_gates_live_only_in_the_crate_root(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for relative in tracked_source::files(&subject.root)? {
        let Some(path) = relative.to_str() else {
            continue;
        };
        if !path.starts_with(LIBRARY_SOURCE)
            || !path.ends_with(".rs")
            || path == PROFILE_DECLARATIONS
        {
            continue;
        }
        let source = fs::read_to_string(subject.root.join(&relative))
            .with_context(|| format!("failed to read {path}"))?;
        violations.extend(gated_lines(&source).into_iter().map(|line| {
            Violation::new(format!(
                "{path}:{line} gates code on a Cargo feature; a profile changes which modules \
                 `{PROFILE_DECLARATIONS}` declares as public, never what a module compiles"
            ))
        }));
    }
    Ok(violations)
}

/// The 1-based lines of `source` that gate anything on a Cargo feature.
///
/// The whole predicate is read rather than its first token, so a feature nested
/// under `all`, `any` or `not` - or wrapped across lines - is the same finding
/// as a bare one. Comments and string literals are read too: a rule that
/// rejects a spelling has no reason to let it survive in prose beside the code
/// that would use it.
fn gated_lines(source: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    for gate in GATES {
        for (start, _) in source.match_indices(gate) {
            let predicate = balanced(&source[start + gate.len() - 1..]);
            if predicate.contains(PROFILE_PREDICATE) {
                lines.push(source[..start].matches('\n').count() + 1);
            }
        }
    }
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// The text inside the parentheses `source` opens with.
fn balanced(source: &str) -> &str {
    let mut depth = 0_usize;
    for (offset, character) in source.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &source[1..offset];
                }
            }
            _ => {}
        }
    }
    &source[1.min(source.len())..]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every spelling of a profile gate is one finding, nested predicates
    /// included; a conditional that is not about a Cargo feature is not.
    #[test]
    fn a_profile_gate_is_recognized_however_the_predicate_is_written() {
        let gated = concat!(
            "//! A doc comment.\n",                                 // 1
            "#[cfg(feature = \"authoring\")]\n",                    // 2
            "pub mod authoring;\n",                                 // 3
            "#[cfg_attr(feature = \"session\", doc = \"host\")]\n", // 4
            "pub struct Session;\n",                                // 5
            "#[cfg(all(feature = \"supervisor\", unix))]\n",        // 6
            "fn host() {}\n",                                       // 7
            "#[cfg(not(any(\n",                                     // 8
            "    feature = \"session\",\n",                         // 9
            "    feature = \"supervisor\",\n",                      // 10
            ")))]\n",                                               // 11
            "mod participant_only;\n",                              // 12
        );
        assert_eq!(gated_lines(gated), [2, 4, 6, 8]);

        let clean = concat!(
            "//! The `authoring` profile publishes this module.\n",
            "#[cfg(test)]\n",
            "mod tests {}\n",
            "#[cfg(unix)]\n",
            "fn only_on_unix() {}\n",
            "const FEATURE: &str = \"authoring\";\n",
        );
        assert!(gated_lines(clean).is_empty(), "{:?}", gated_lines(clean));
    }

    /// The exemption is one path, so it stops applying the moment the profile
    /// declarations move - and the crate root would then report itself.
    #[test]
    fn the_exempt_declaration_file_is_the_crate_root() {
        assert!(PROFILE_DECLARATIONS.starts_with(LIBRARY_SOURCE));
        assert!(PROFILE_DECLARATIONS.ends_with("/lib.rs"));
    }
}
