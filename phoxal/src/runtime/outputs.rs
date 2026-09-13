//! Output metadata and the nested attribute-macro namespace.
//!
//! The output collector records source-authored roles and bounds.  It does not
//! publish, start an activation, or retain a batch.  The runner owns those
//! actions after a complete invocation has been accepted.

use super::StepContext;
use super::input::TransportValue;
use crate::port::PortSignature;

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

impl OutputKind {
    /// Returns the stable lower-case spelling used by artifact contracts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Sample => "sample",
            Self::Event => "event",
            Self::Stream => "stream",
            Self::Setpoint => "setpoint",
            Self::Read => "read",
            Self::Reply => "reply",
            Self::Activate => "activate",
            Self::Operation => "operation",
        }
    }
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
    /// Complete generated descriptor identity for a bound served port.
    pub port_signature: Option<PortSignature>,
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

/// Erased worker used by a generated local Operation binding.
pub type OperationWorker =
    Box<dyn Fn(TransportValue) -> crate::Result<TransportValue> + Send + Sync>;

/// One received request routed to a generated Read handler.
pub struct RuntimeReadRequest {
    /// The generated handler method name selected by the request key.
    pub field: &'static str,
    /// The raw bounded Protobuf sample and its delivery metadata.
    pub sample: super::transport::WireSample,
}

/// The serialized runtime-owned side effects prepared by generated output
/// bindings.  Implementations only stage work during candidate preparation;
/// the runner dispatches it after the invocation and its output reservation
/// have been accepted.
pub trait RuntimeWorkSink {
    /// Stage one managed activation.  A worker makes the activation local to
    /// this runner's operation owner; without a worker the request is sent to
    /// the exact graph-resolved remote port after output acceptance.
    #[allow(clippy::too_many_arguments)]
    fn activate(
        &mut self,
        field: &'static str,
        key: TransportValue,
        request: TransportValue,
        worker: Option<OperationWorker>,
        request_codec: Option<crate::port::PortCodec>,
        timeout_ms: Option<u64>,
        refresh_every_steps: Option<u64>,
        cancel_grace_ms: Option<u64>,
        context: StepContext,
    ) -> crate::Result<()>;

    /// Stage a generated Read handler response for publication after the
    /// current candidate is accepted.
    fn push_read_reply(&mut self, output: super::transport::PreparedOutput) -> crate::Result<()>;
}

/// Metadata emitted for an output struct collector.
pub trait OutputSet: 'static {
    /// Declared transient fields in source order.
    const FIELDS: &'static [OutputField];

    /// Encode the complete fresh output value using its generated port
    /// descriptors.  The runner calls this during output reservation, before
    /// it commits the invocation's state or schedule advancement.
    fn encode_transport(
        &self,
        _context: StepContext,
        _resolve_input_port: &dyn Fn(&str) -> Option<PortSignature>,
        _source: &str,
    ) -> crate::Result<Vec<super::transport::PreparedOutput>> {
        Ok(Vec::new())
    }
}

impl OutputSet for () {
    const FIELDS: &'static [OutputField] = &[];
}

/// Metadata emitted for an inherent service output collector.
pub trait OutputBindings: super::Runtime + 'static {
    /// Declared projection, read, activation, and worker methods.
    const FIELDS: &'static [OutputField];

    /// Encode state and setpoint projections after the next state has been
    /// computed but before the owner commits the invocation's output
    /// reservation.
    fn encode_transport(
        &self,
        _state: &Self::State,
        _context: StepContext,
        _resolve_input_port: &dyn Fn(&str) -> Option<PortSignature>,
        _source: &str,
    ) -> crate::Result<Vec<super::transport::PreparedOutput>> {
        Ok(Vec::new())
    }

    /// Prepare managed Read/Request activations and local Operation work for
    /// the candidate next state.  The default keeps runtimes with only
    /// ordinary publications transport-free.
    fn prepare_work(
        &self,
        _state: &Self::State,
        _context: StepContext,
        _sink: &mut dyn RuntimeWorkSink,
    ) -> crate::Result<()> {
        Ok(())
    }

    /// Serve Read requests received before this candidate.  Read handlers run
    /// through the generated service binding and return a prepared reply that
    /// is published only after the candidate is accepted.
    fn serve_reads(
        &self,
        _state: &Self::State,
        _context: StepContext,
        _requests: &mut Vec<RuntimeReadRequest>,
        _source: &str,
        _sink: &mut dyn RuntimeWorkSink,
    ) -> crate::Result<()> {
        Ok(())
    }
}

// Attribute macros share the Rust module namespace with this metadata.  The
// re-exports are intentionally nested so authors can spell the normative
// `#[phoxal::runtime::outputs::state(...)]` path.
#[doc(hidden)]
pub use phoxal_macros::{
    activate, event, operation, outputs, read, reply, sample, setpoint, state, stream,
};
