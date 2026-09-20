//! The ownership rule for unit-test modules below crate `src/` trees.
//!
//! A generic `tests` bucket hides which boundary a large suite proves.
//! Test modules must instead be named for the behavior or boundary they own.

use std::ffi::OsStr;
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
    below_source == [OsStr::new("tests.rs")]
        || below_source == [OsStr::new("tests"), OsStr::new("mod.rs")]
}

/// A crate-root generic unit-test module must be named for the boundary it
/// proves.
/// A `tests` module nested beneath a named owner such as `runtime` or `project`
/// already has an explicit boundary.
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
