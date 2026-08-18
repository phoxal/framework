//! Exact authored document schemas.
//!
//! A schema tag names the *source language* a document is written in. It is a
//! generation of an authored grammar and nothing else: it is not a framework
//! compatibility identity, it negotiates nothing between binaries, and no
//! runtime reads it. `FrameworkVersion` remains the only negotiated
//! compatibility identity.
//!
//! Each document kind owns its version independently, so `robot.yaml` v1 can be
//! introduced without forcing `component.yaml` or `simulation.yaml` to advance.
//! A new version is a new DTO beside the existing one, with its own `normalize`
//! into `normalized`, the one version-independent form the compiler consumes.
//! That normalized form is crate-internal, so a source generation can be added
//! without changing what this crate or `phoxal-model` exposes, and without a
//! second copy of the compiler.
//!
//! Runtime code reads the canonical runtime model. These modules are for
//! schema-aware tools, fixtures and the compiler itself.
//!
//! Every document kind is reached the same way, through associated functions on
//! its `Manifest`: `parse` for exact text, `load` for a path, `write_to_dir` to
//! write it back. All of them validate.
//!
//! `document` and `strict_yaml` are the shared mechanics behind that, and are
//! private: the vocabulary they own is named through this module, which is its
//! one canonical path.

mod document;
mod strict_yaml;

pub mod component;
pub mod robot;
pub mod simulation;

pub use document::{ComposeError, DocumentKind, Origin, SourceError, Violations};
pub use strict_yaml::{ReservedMarker, StrictYamlError};

pub(crate) use document::document_path;

/// Adopt an authored value into its canonical counterpart.
///
/// The two shapes are serde-compatible by construction: the authored DTO's job
/// is to add defaults and permissive spellings on the way in, and the canonical
/// value is what is left once those are resolved. Going through JSON is what
/// keeps that "same wire, different obligations" relationship explicit rather
/// than hiding it in a hand-written field-by-field copy that would silently
/// drift.
///
/// This is a normalizer's tool, not a migration framework: it only ever adopts
/// one schema generation's own shape into the canonical one, and a generation
/// whose shape has diverged writes the conversion out instead.
pub(crate) fn transcode<T: serde::Serialize, U: serde::de::DeserializeOwned>(
    authored: &T,
    what: &'static str,
) -> Result<U, crate::CompileError> {
    let value =
        serde_json::to_value(authored).map_err(|source| crate::CompileError::Transcode {
            authored: what,
            source,
        })?;
    serde_json::from_value(value).map_err(|source| crate::CompileError::Transcode {
        authored: what,
        source,
    })
}
