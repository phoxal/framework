//! Output metadata and the nested attribute-macro namespace.
//!
//! The output collector records source-authored roles and bounds.  It does not
//! publish, start an activation, or retain a batch.  The runner owns those
//! actions after a complete invocation has been accepted.

pub mod activation;
pub mod read;

use super::StepContext;
use super::input::TransportValue;
use crate::contract::{Call, MethodSignature, Withdraw};
use crate::port::PortSignature;
use activation::ActivationKey;
use prost::Message;
use std::marker::PhantomData;

const GENERATED_OPERATION_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Fresh generated operations staged by one runtime invocation.
///
/// Values remain inert until the runner accepts the complete invocation and
/// its output reservation.
#[derive(Debug, Default)]
pub struct Outputs {
    operations: Vec<GeneratedOperation>,
}

#[derive(Debug)]
enum GeneratedOperation {
    Send {
        instance: &'static str,
        signature: MethodSignature,
        ticket: u64,
        payload: Vec<u8>,
    },
    Withdraw {
        instance: &'static str,
        signature: MethodSignature,
    },
}

/// Typed identity of one staged call result.
#[derive(Debug, Eq, PartialEq)]
pub struct CallTicket<Response> {
    id: u64,
    response: PhantomData<fn() -> Response>,
}

impl<Response> Copy for CallTicket<Response> {}

impl<Response> Clone for CallTicket<Response> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Response> CallTicket<Response> {
    /// Execution-local identity used by generated completion inputs.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }
}

/// Generated operation that can enter a runtime output transaction.
pub trait GeneratedSend {
    type Response;

    fn append_to(self, outputs: &mut Outputs, ticket: u64) -> crate::Result<()>;
}

impl<Request, Response> GeneratedSend for Call<Request, Response>
where
    Request: Message,
{
    type Response = Response;

    fn append_to(self, outputs: &mut Outputs, ticket: u64) -> crate::Result<()> {
        let (instance, signature, request) = self.into_parts();
        let payload = request.encode_to_vec();
        if payload.len() as u64 > GENERATED_OPERATION_MAX_BYTES {
            return Err(crate::anyhow!(
                "generated call {}.{} encoded {} bytes, exceeding the {} byte bound",
                signature.service,
                signature.method,
                payload.len(),
                GENERATED_OPERATION_MAX_BYTES,
            ));
        }
        outputs.operations.push(GeneratedOperation::Send {
            instance,
            signature,
            ticket,
            payload,
        });
        Ok(())
    }
}

impl<Request, Response> GeneratedSend for Withdraw<Request, Response> {
    type Response = ();

    fn append_to(self, outputs: &mut Outputs, _ticket: u64) -> crate::Result<()> {
        outputs.operations.push(GeneratedOperation::Withdraw {
            instance: self.instance(),
            signature: self.signature(),
        });
        Ok(())
    }
}

impl Outputs {
    /// Stage one generated call, lease renewal, or withdrawal.
    pub fn send<O>(
        &mut self,
        context: &StepContext,
        operation: O,
    ) -> crate::Result<CallTicket<O::Response>>
    where
        O: GeneratedSend,
    {
        let index = u32::try_from(self.operations.len())
            .map_err(|_| crate::anyhow!("generated runtime operation count overflowed"))?;
        let id = (context.invocation_index() << 32) | u64::from(index);
        operation.append_to(self, id)?;
        Ok(CallTicket {
            id,
            response: PhantomData,
        })
    }
}

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
        key: ActivationKey,
        request: TransportValue,
        worker: Option<OperationWorker>,
        timeout_ms: Option<u64>,
        refresh_every_steps: Option<u64>,
        cancel_grace_ms: Option<u64>,
        context: StepContext,
    ) -> crate::Result<()>;

    /// Retire local interest after acceptance without undoing remote effects.
    fn deactivate(&mut self, field: &'static str) -> crate::Result<()>;
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

impl OutputSet for Outputs {
    const FIELDS: &'static [OutputField] = &[];

    fn encode_transport(
        &self,
        context: StepContext,
        _resolve_input_port: &dyn Fn(&str) -> Option<PortSignature>,
        source: &str,
    ) -> crate::Result<Vec<super::transport::PreparedOutput>> {
        self.operations
            .iter()
            .enumerate()
            .map(|(index, operation)| match operation {
                GeneratedOperation::Send {
                    instance,
                    signature,
                    ticket,
                    payload,
                } => {
                    let port = generated_port_signature(
                        *signature,
                        if signature.lease.is_some() {
                            crate::port::PortKind::Setpoint
                        } else {
                            crate::port::PortKind::Commands
                        },
                    );
                    let sequence = *ticket;
                    if let Some(lease) = signature.lease {
                        super::transport::PreparedOutput::encoded_response(
                            port,
                            payload.clone(),
                            GENERATED_OPERATION_MAX_BYTES,
                            super::transport::setpoint_metadata(
                                source,
                                context,
                                sequence,
                                lease.valid_for_ms(),
                            ),
                        )
                        .map(|output| output.for_instance(*instance))
                        .map(super::transport::PreparedOutput::generated_operation)
                        .map_err(Into::into)
                    } else {
                        super::transport::PreparedOutput::encoded_request(
                            port,
                            payload.clone(),
                            GENERATED_OPERATION_MAX_BYTES,
                            super::transport::request_metadata(
                                source,
                                source,
                                context,
                                sequence,
                                context.invocation_index().saturating_add(1),
                                0,
                            ),
                        )
                        .map(|output| output.for_instance(*instance))
                        .map(super::transport::PreparedOutput::generated_operation)
                        .map_err(Into::into)
                    }
                }
                GeneratedOperation::Withdraw {
                    instance,
                    signature,
                } => {
                    let sequence = (context.invocation_index() << 32)
                        | u64::try_from(index).unwrap_or(u64::MAX);
                    let lease = signature.lease.ok_or_else(|| {
                        crate::anyhow!(
                            "withdrawal {}.{} has no contract lease",
                            signature.service,
                            signature.method,
                        )
                    })?;
                    Ok(super::transport::PreparedOutput::withdrawal(
                        generated_port_signature(*signature, crate::port::PortKind::Setpoint),
                        super::transport::setpoint_metadata(
                            source,
                            context,
                            sequence,
                            lease.valid_for_ms(),
                        ),
                    )
                    .for_instance(*instance)
                    .generated_operation())
                }
            })
            .collect()
    }
}

fn generated_port_signature(
    signature: MethodSignature,
    kind: crate::port::PortKind,
) -> PortSignature {
    PortSignature::from_method(signature, kind)
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

    /// Capture owned immutable views from initialized or candidate State.
    /// The runner exposes these views only after acceptance.
    fn prepare_read_views(
        self: std::sync::Arc<Self>,
        _state: &Self::State,
    ) -> crate::Result<Vec<read::ReadView>> {
        Ok(Vec::new())
    }
}

// Attribute macros share the Rust module namespace with this metadata.  The
// re-exports are intentionally nested so authors can spell the normative
// `#[phoxal::runtime::outputs::state(...)]` path.
pub use phoxal_macros::{
    activate, event, operation, outputs, read, reply, sample, setpoint, state, stream,
};
