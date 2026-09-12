//! Output metadata and the nested attribute-macro namespace.
//!
//! The output collector records source-authored roles and bounds.  It does not
//! publish, start an activation, or retain a batch.  The runner owns those
//! actions after a complete invocation has been accepted.

/// The output role declared by one field or method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputKind {
    /// Periodic state projection.
    State,
    /// Captured sample batch.
    Sample,
    /// Event batch.
    Event,
    /// Ordered stream batch.
    Stream,
    /// Replaceable setpoint projection.
    Setpoint,
    /// Offered immutable read.
    Read,
    /// Correlated command reply batch.
    Reply,
    /// An activation selector.
    Activate,
    /// A finite operation worker.
    Operation,
}

/// Compile-time metadata for one collected output role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputField {
    /// Private Rust field or method name.
    pub name: &'static str,
    /// Output role.
    pub kind: OutputKind,
    /// Public port name for served outputs.
    pub port: Option<&'static str>,
    /// Local input field selected by a reply, activation, or worker.
    pub input: Option<&'static str>,
    /// Projection method selected by an offered read.
    pub project: Option<&'static str>,
    /// Item or encoded-byte bound.
    pub max_items: Option<u64>,
    /// Encoded-byte bound.
    pub max_bytes: Option<u64>,
    /// Encoded request bound for an offered read.
    pub max_request_bytes: Option<u64>,
    /// Periodic projection cadence.
    pub every_steps: Option<u64>,
    /// Whether publication is gated by payload/stamp changes.
    pub on_change: bool,
    /// Whether initialized state is published before the first step.
    pub bootstrap: bool,
    /// Setpoint validity duration.
    pub valid_for_ms: Option<u64>,
    /// Host-monotonic activation or read-handler timeout.
    pub timeout_ms: Option<u64>,
    /// Operation retirement grace.
    pub cancel_grace_ms: Option<u64>,
}

/// Metadata emitted for an output struct collector.
pub trait OutputSet: 'static {
    /// Declared transient fields in source order.
    const FIELDS: &'static [OutputField];
}

/// Metadata emitted for an inherent service output collector.
pub trait OutputBindings: 'static {
    /// Declared projection, read, activation, and worker methods.
    const FIELDS: &'static [OutputField];
}

// Attribute macros share the Rust module namespace with this metadata.  The
// re-exports are intentionally nested so authors can spell the normative
// `#[phoxal::runtime::outputs::state(...)]` path.
#[doc(hidden)]
pub use phoxal_macros::{
    activate, event, operation, outputs, read, reply, sample, setpoint, state, stream,
};
