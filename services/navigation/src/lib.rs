//! Official navigation Runtime implementation.
//!
//! Robot brains may import this library for direct state-transition tests while
//! the package's binary target runs the identical implementation in a process.

mod service;

pub use service::*;
