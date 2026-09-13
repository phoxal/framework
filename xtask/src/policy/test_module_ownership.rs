//! The ownership rule for unit-test modules below crate `src/` trees.
//!
//! A generic `tests` bucket hides which boundary a large suite proves.
//! Test modules must instead be named for the behavior or boundary they own.

use std::path::Path;

use anyhow::Result;

use super::tracked_source;
use super::{Subject, Violation};

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

/// Every generic unit-test module is named for the boundary it proves, so
/// ownership remains visible when the crate grows.
pub(super) fn unit_test_modules_have_an_explicit_owner(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    Ok(tracked_source::files(&subject.root)?
        .into_iter()
        .filter(|path| is_generic_source_test_module(path))
        .map(|path| {
            Violation::new(format!(
                "{} must be named for the boundary it proves",
                path.display()
            ))
        })
        .collect())
}
