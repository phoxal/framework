//! Canonical runtime model and opt-in authored source documents.
//!
//! Runtime consumers use [`Robot`] and the unversioned canonical modules.
//! Schema-aware tools and loaders can name exact YAML grammars through
//! [`source`].

pub mod component;
pub mod robot;
pub mod simulation;
pub mod source;
pub mod structure;

pub use robot::Robot;
