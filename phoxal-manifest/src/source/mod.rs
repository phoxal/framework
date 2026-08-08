//! Exact authored document schemas.
//!
//! Each document kind owns its version independently, so `robot.yaml` v1 can be
//! introduced without forcing `component.yaml` or `simulation.yaml` to advance.
//! A new version is a new DTO beside the existing one, with its own explicit
//! conversion into the same canonical builder input; that normalized input is
//! crate-internal so a source version can be added without changing what
//! `phoxal-model` exposes.
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
