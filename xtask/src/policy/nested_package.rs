//! Forbidden: a nested `Cargo.toml` under a tool-owned product.
//!
//! Plan §9 / Unit 3.4 say: tool-owned implementation that exists only to
//! implement one tool must be a Rust module of that tool, not another
//! Cargo package. Adding a `Cargo.toml` directly under
//! `phoxal/cargo/` re-creates that prohibited shape:
//! `cargo-phoxal` then becomes a workspace that depends on a sibling
//! tool implementation package, instead of a binary whose private
//! implementation lives behind `mod`.
//!
//! The rule walks `phoxal/cargo/` and fails if any direct child has a
//! `Cargo.toml`. Deeper nesting is not asserted here because the
//! `library_crate_list_matches_the_workspace_members` rule already
//! rejects hidden second packages through the workspace graph.

use super::{Subject, Violation};

/// Walk `phoxal/cargo/` and report every direct child directory that itself
/// contains a `Cargo.toml`. The tool's own manifest is the product itself;
/// only nested children are reported.
/// The rule exists because plan §9 / Unit 3.4 say tool-owned private
/// implementation must be a Rust module of the tool, not another Cargo
/// package nested underneath it.
pub(crate) fn no_nested_private_cargo_package_under_a_tool(
    subject: &Subject,
) -> Result<Vec<Violation>, anyhow::Error> {
    let mut violations = Vec::new();
    let tool_root = subject.root.join("phoxal/cargo");
    let entries = match std::fs::read_dir(&tool_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(violations),
        Err(error) => {
            return Err(anyhow::Error::from(error)
                .context(format!("cannot enumerate {}", tool_root.display())));
        }
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let nested_manifest = path.join("Cargo.toml");
        if nested_manifest.is_file() {
            let relative = nested_manifest
                .strip_prefix(&subject.root)
                .unwrap_or(&nested_manifest)
                .to_string_lossy()
                .into_owned();
            violations.push(Violation::new(format!(
                "{relative} is forbidden: a tool-owned product must keep its private \
                 implementation as Rust modules of the tool, not as a nested Cargo package."
            )));
        }
    }
    Ok(violations)
}
