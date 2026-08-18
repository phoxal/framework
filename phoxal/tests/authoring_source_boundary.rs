// This target is test code end to end, which the workspace panic gate exempts
// through `clippy.toml`. That exemption only reaches code lexically inside a
// `#[test]` function, so the source-walking helpers the tests share would trip
// a gate that was never meant to cover them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The compiler below the normalization boundary names no schema generation.
//!
//! A type system cannot state this rule: a versioned DTO and the normalized
//! value it produces are both ordinary Rust types, and nothing stops a later
//! change from reaching past the normalizer and reading `source::robot::v0`
//! again. So the rule is checked as text, the way the workspace's other
//! structural rules are.
//!
//! Exempt are the schemas themselves, which are what a generation *is*, and the
//! editor-schema generator, whose whole job is to name the versioned document
//! roots. Everything else under `src/` is below the boundary and must be able
//! to compile a robot without knowing which source language authored it.

use std::path::{Path, PathBuf};

/// Files that legitimately name a schema generation, relative to `src/`.
///
/// `source/` holds the versioned DTOs and their normalizers - the boundary
/// itself. `schema.rs` derives the published editor schemas from the versioned
/// document roots, which is the one other place a version is the subject.
const EXEMPT: [&str; 2] = ["source", "schema.rs"];

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(directory: &Path, exempt: &[&str], found: &mut Vec<PathBuf>) {
    let mut entries = std::fs::read_dir(directory)
        .expect("the crate's source tree is readable")
        .map(|entry| entry.expect("a readable directory entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a UTF-8 source path");
        if exempt.contains(&name) {
            continue;
        }
        if path.is_dir() {
            rust_files(&path, exempt, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// Whether a token is a schema-generation module or variant name (`v0`, `V1`).
fn is_generation(token: &str) -> bool {
    let mut characters = token.chars();
    characters
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case(&'v'))
        && token.len() > 1
        && characters.all(|character| character.is_ascii_digit())
}

/// Every path segment in `source` that names a schema generation, with the line
/// it appears on.
fn generation_paths(source: &str) -> Vec<String> {
    let identifier = |character: char| character.is_ascii_alphanumeric() || character == '_';
    let mut found = Vec::new();
    for (number, line) in source.lines().enumerate() {
        for (index, _) in line.match_indices("::") {
            let before = line[..index]
                .rfind(|character| !identifier(character))
                .map_or(&line[..index], |boundary| &line[boundary + 1..index]);
            let rest = &line[index + 2..];
            let after = rest
                .find(|character| !identifier(character))
                .map_or(rest, |boundary| &rest[..boundary]);
            if is_generation(before) || is_generation(after) {
                found.push(format!("line {}: {}", number + 1, line.trim()));
            }
        }
    }
    found
}

#[test]
fn the_compiler_below_the_boundary_names_no_schema_generation() {
    let root = source_root();
    for exempt in EXEMPT {
        assert!(
            root.join(exempt).exists(),
            "{exempt} is exempt from the boundary rule but no longer exists"
        );
    }

    let mut files = Vec::new();
    rust_files(&root, &EXEMPT, &mut files);
    assert!(
        files.len() >= 4,
        "the boundary rule found almost nothing to check: {files:?}"
    );

    let mut violations = Vec::new();
    for path in files {
        let source = std::fs::read_to_string(&path).expect("a readable source file");
        for occurrence in generation_paths(&source) {
            violations.push(format!("{}: {occurrence}", path.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "the compiler below the normalization boundary must not name a schema generation; \
         normalize the fact instead: {violations:#?}"
    );
}

/// The rule can actually fail, so its absence of violations means something.
#[test]
fn the_boundary_rule_recognizes_a_generation_path() {
    assert_eq!(
        generation_paths("let source::robot::v0::Manifest::V0(body) = value;"),
        [
            "line 1: let source::robot::v0::Manifest::V0(body) = value;",
            "line 1: let source::robot::v0::Manifest::V0(body) = value;",
            "line 1: let source::robot::v0::Manifest::V0(body) = value;",
        ]
    );
    assert_eq!(
        generation_paths("use crate::source::robot::v0;"),
        ["line 1: use crate::source::robot::v0;"]
    );
    assert!(generation_paths("// robot.yaml v0 is the current grammar").is_empty());
    assert!(generation_paths("let value = other::vector::new();").is_empty());
}
