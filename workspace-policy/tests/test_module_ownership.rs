//! The ownership rule for unit-test modules below crate `src/` trees.
//!
//! A generic `tests` bucket hides which boundary a large suite proves. The one
//! deliberate survivor sits below `participant/runner/`: its directory is the
//! owner, and splitting the runner implementation from its test fixtures keeps
//! that already-focused module readable.

use std::path::Path;

use anyhow::Result;
use workspace_policy::{tracked_source, workspace_root};

const OWNED_DIRECTORY_TEST_MODULES: [&str; 1] = ["phoxal/src/participant/runner/tests.rs"];

fn is_generic_source_test_module(path: &Path) -> bool {
    let components = path
        .components()
        .map(|component| component.as_os_str())
        .collect::<Vec<_>>();
    let Some(source_index) = components.iter().position(|component| *component == "src") else {
        return false;
    };
    let below_source = &components[source_index + 1..];
    below_source.last().is_some_and(|name| *name == "tests.rs")
        || below_source.ends_with(&["tests".as_ref(), "mod.rs".as_ref()])
}

/// Every otherwise-generic unit-test module is named for the boundary it
/// proves, so ownership remains visible when the crate grows.
#[test]
fn unit_test_modules_have_an_explicit_owner() -> Result<()> {
    let root = workspace_root()?;
    let tracked = tracked_source::files(root)?;
    for allowed in OWNED_DIRECTORY_TEST_MODULES {
        assert!(
            tracked.iter().any(|path| path == Path::new(allowed)),
            "{allowed} is allowlisted but no longer exists"
        );
    }
    let violations = tracked
        .into_iter()
        .filter(|path| is_generic_source_test_module(path))
        .filter(|path| {
            !OWNED_DIRECTORY_TEST_MODULES
                .iter()
                .any(|allowed| path == Path::new(allowed))
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "unit-test modules must be named for the boundary they prove: {violations:?}"
    );
    Ok(())
}
