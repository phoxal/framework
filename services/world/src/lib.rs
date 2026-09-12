//! Official world Runtime implementation.
//!
//! Robot brains and reference adapters may import this library for direct
//! state-transition tests while the package's binary target runs the same
//! implementation in a process.

mod service;

pub use service::*;
