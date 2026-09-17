//! SDK public surface must not re-export moved server-state types.
//!
//! Plan §8 / Unit 4 closed the SDK / supervisor ownership boundary. After
//! that commit, the SDK re-exports only client types and runtime-shared
//! protocol vocabulary. None of the supervisor's authoritative state may
//! leak through the SDK's public re-export surface. This rule enforces that
//! invariant at compile time by scanning the SDK's two published re-export
//! sites (`phoxal/src/session/mod.rs` and the parent of
//! `phoxal/src/communication_transport/`) for the type names that were moved
//! to the supervisor.
//!
//! The forbidden names are the type identifiers owned by
//! `phoxal-supervisor::runtime::adapter`,
//! `phoxal-supervisor::runtime::session_table`, and
//! `phoxal-supervisor::runtime::transport`. Adding any of them to
//! `phoxal::session::pub use` or to `phoxal::communication_transport`'s
//! top-level `pub use` would silently re-open the leak the migration
//! closed.
//!
//! The scan is line-local: the rule reads each tracked source file
//! character-by-character, looking for whole-word occurrences of any
//! forbidden name inside the SDK's two re-export sites. Comments and
//! `// forbidden by this rule` prose are explicitly ignored because the
//! rule itself needs to mention these names to describe what it forbids.

use super::{Subject, Violation};
use std::fs;

/// One symbol name that must not appear in the SDK's re-export surface.
const FORBIDDEN_SDK_REEXPORTS: &[&str] = &[
    // Adapter types — moved to supervisor::runtime::adapter in Unit 4.2.
    "SupervisorAdapter",
    "SupervisorAdapterError",
    "AdapterLimits",
    "BindingContext",
    "BindingId",
    "ExecutionDefinition",
    "ServicePorts",
    "SimulationDefinition",
    "SimulationProviderDefinition",
    "Invalidation",
    // Session table — moved to supervisor::runtime::session_table in
    // Unit 4.2.
    "SessionTable",
    "LogicalSession",
    "SessionTableError",
    // Server transport — moved to supervisor::runtime::transport in
    // Unit 4.3.
    "PublicSessionServer",
    "PrincipalPolicy",
    "PublicSessionBackend",
    "PublicSimulationBackend",
    "PublicSimulationContext",
    "PublicBindingContext",
    "PublicBackendOutcome",
    "PublicBackendSubscription",
    "SimulationAuthority",
    "OperationServerContext",
    "UnavailableBackend",
    "UnavailableSimulationBackend",
];

/// Scan every forbidden-name mention inside a file. Line-local matches only:
/// a line containing a comment (`//`) or a doc comment (`///` or `//!`) is
/// skipped. Otherwise the line is searched for whole-word occurrences of any
/// forbidden name.
fn forbidden_in_line(line: &str) -> Vec<&'static str> {
    if line.contains("//") {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            return Vec::new();
        }
    }
    FORBIDDEN_SDK_REEXPORTS
        .iter()
        .copied()
        .filter(|name| contains_word(line, name))
        .collect()
}

/// Whether `line` contains `word` as a whole word — not as a prefix or
/// substring. Rust identifiers are ASCII alphanumeric or `_`, so any
/// non-identifier neighbour counts as a boundary.
fn contains_word(line: &str, word: &str) -> bool {
    let bytes = line.as_bytes();
    let needle = word.as_bytes();
    let mut index = 0;
    while index + needle.len() <= bytes.len() {
        if &bytes[index..index + needle.len()] == needle {
            let before_ok = index == 0
                || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_');
            let after_index = index + needle.len();
            let after_ok = after_index == bytes.len()
                || !(bytes[after_index].is_ascii_alphanumeric() || bytes[after_index] == b'_');
            if before_ok && after_ok {
                return true;
            }
        }
        index += 1;
    }
    false
}

/// Two SDK re-export sites must keep the moved server-state type names out
/// of their `pub use` lists:
///
///   * `phoxal/src/session/mod.rs` — convenience re-exports for consumers
///   * `phoxal/src/communication_transport.rs` — re-export of the client
///     half. The server half was moved to
///     `phoxal-supervisor::runtime::transport::*` and must not re-appear
///     here.
///
/// The rule fires on every occurrence (the first re-introduction of a
/// moved name is enough to break the invariant). The path-relative file
/// reference is what the report shows; the matching line is included so
/// the developer can find the offending site immediately.
pub(crate) fn the_sdk_keeps_server_state_out_of_its_public_surface(
    subject: &Subject,
) -> Result<Vec<Violation>, anyhow::Error> {
    let mut violations = Vec::new();
    let targets = [
        "phoxal/src/session/mod.rs",
        "phoxal/src/communication_transport.rs",
    ];
    for relative in targets {
        let path = subject.root.join(relative);
        let source = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                violations.push(Violation::new(format!(
                    "{relative}: cannot read file: {error:#}"
                )));
                continue;
            }
        };
        for (line_number, line) in source.lines().enumerate() {
            for forbidden in forbidden_in_line(line) {
                violations.push(Violation::new(format!(
                    "{relative}:{} re-exports `{forbidden}`, which is owned by the supervisor; \
                     move the import to phoxal-supervisor::runtime::* or remove it",
                    line_number + 1,
                )));
            }
        }
    }
    Ok(violations)
}
