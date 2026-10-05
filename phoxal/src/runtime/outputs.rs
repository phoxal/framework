//! Output metadata and the nested attribute-macro namespace.
//!
//! The output collector records source-authored roles and bounds.  It does not
//! publish, start an activation, or retain a batch.  The runner owns those
//! actions after a complete invocation has been accepted.

pub mod activation;
pub mod read;

use super::StepContext;
use super::input::TransportValue;
use crate::contracts::{Call, MethodSignature, Withdraw};
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
    /// The next per-candidate operation ordinal. Monotonic and never
    /// reused: withdrawing an earlier submission must not let a later one
    /// mint its identity and overwrite its ownership.
    next_ordinal: usize,
    /// Field ownership records for calls staged through the direct
    /// `#[complete]` path in this candidate; promoted into the runtime's
    /// pending-call ledger when the owner accepts the complete candidate.
    direct_owners: Vec<(u128, &'static str)>,
    /// Tickets of calls staged through the tree-owned path in this
    /// candidate with their owning tree generations, promoted with the
    /// same acceptance proof.
    tree_owners: Vec<(u128, u64)>,
}

#[derive(Debug)]
enum GeneratedOperation {
    Send {
        instance: &'static str,
        signature: MethodSignature,
        ticket: u128,
        payload: Vec<u8>,
    },
    Withdraw {
        instance: &'static str,
        signature: MethodSignature,
    },
}

impl GeneratedOperation {
    /// The correlated ticket of a staged call submission, when this
    /// operation is one.
    fn ticket(&self) -> Option<u128> {
        match self {
            GeneratedOperation::Send { ticket, .. } => Some(*ticket),
            GeneratedOperation::Withdraw { .. } => None,
        }
    }
}

/// Typed identity of one staged call result.
#[derive(Debug, Eq, PartialEq)]
pub struct CallTicket<Response> {
    id: u128,
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
    pub const fn id(&self) -> u128 {
        self.id
    }
}

/// Generated operation that can enter a runtime output transaction.
pub trait GeneratedSend {
    type Response;

    fn append_to(self, outputs: &mut Outputs, ticket: u128) -> crate::Result<()>;
}

impl<Request, Response> GeneratedSend for Call<Request, Response>
where
    Request: Message,
{
    type Response = Response;

    fn append_to(self, outputs: &mut Outputs, ticket: u128) -> crate::Result<()> {
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

    fn append_to(self, outputs: &mut Outputs, _ticket: u128) -> crate::Result<()> {
        outputs.operations.push(GeneratedOperation::Withdraw {
            instance: self.instance(),
            signature: self.signature(),
        });
        Ok(())
    }
}

/// The largest invocation index one execution's tickets can carry: the
/// 32-bit half below the execution epoch. A single unbroken execution may
/// run about 2.7 years at a 20 ms period before the checked error fires;
/// any reinitialization starts a fresh epoch and a fresh half.
pub const MAX_TICKET_INVOCATION: u64 = u32::MAX as u64;

/// Composes one generated-call ticket: the execution epoch owns the high
/// 64 bits, the invocation index the next 32, and the per-invocation
/// operation ordinal the low 32. Every execution initialization draws a
/// process-unique epoch, so no reinitialization or successor execution —
/// on any thread — can mint or claim an id another execution minted. Each
/// field is checked: a value past its width would silently wrap onto
/// another call's ticket, so it is rejected instead of shifted.
/// The wire-facing 64-bit sequence of one ticket: the low half, which is
/// unique within an execution and is replaced by the transport command id
/// for call submissions at reservation.
fn wire_sequence(ticket: u128) -> u64 {
    u64::try_from(ticket & u128::from(u64::MAX)).unwrap_or(u64::MAX)
}

pub fn compose_call_ticket(epoch: u64, invocation: u64, operation: usize) -> crate::Result<u128> {
    if invocation > MAX_TICKET_INVOCATION {
        return Err(crate::anyhow!(
            "generated runtime invocation index overflowed the ticket space"
        ));
    }
    let operation = u32::try_from(operation)
        .map_err(|_| crate::anyhow!("generated runtime operation count overflowed"))?;
    crate::Result::Ok(
        (u128::from(epoch) << 64) | (u128::from(invocation) << 32) | u128::from(operation),
    )
}

impl Outputs {
    /// Stage one generated call, lease renewal, or withdrawal under the
    /// direct ticket layout.
    pub fn send<O>(
        &mut self,
        context: &StepContext,
        operation: O,
    ) -> crate::Result<CallTicket<O::Response>>
    where
        O: GeneratedSend,
    {
        let ordinal = self.next_ordinal;
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or_else(|| crate::anyhow!("generated runtime operation count overflowed"))?;
        let id = compose_call_ticket(
            context.execution_epoch(),
            context.invocation_index(),
            ordinal,
        )?;
        operation.append_to(self, id)?;
        Ok(CallTicket {
            id,
            response: PhantomData,
        })
    }

    /// Takes this candidate's direct-field ownership records.
    pub fn take_direct_owners(&mut self) -> Vec<(u128, &'static str)> {
        std::mem::take(&mut self.direct_owners)
    }

    /// Takes this candidate's tree-owned ticket records with the owning
    /// tree generations.
    pub fn take_tree_owners(&mut self) -> Vec<(u128, u64)> {
        std::mem::take(&mut self.tree_owners)
    }

    /// Withdraws one not-yet-accepted request from this candidate output
    /// transaction: cancellation before acceptance removes the submission
    /// itself, so no remote effect is produced and no ownership is staged
    /// for it. Returns whether a staged operation was removed.
    pub fn withdraw(&mut self, ticket: u128) -> bool {
        let before = self.operations.len();
        self.operations
            .retain(|operation| operation.ticket() != Some(ticket));
        self.direct_owners.retain(|(owned, _)| *owned != ticket);
        self.tree_owners.retain(|(owned, _)| *owned != ticket);
        before != self.operations.len()
    }

    /// Stages one generated call for the direct `#[complete]` handler of
    /// the named call field: the field's ownership record is committed
    /// with the candidate so the completion routes to that handler exactly
    /// once, independently of tree activity.
    pub fn send_direct<O>(
        &mut self,
        context: &StepContext,
        operation: O,
        field: &'static str,
    ) -> crate::Result<CallTicket<O::Response>>
    where
        O: GeneratedSend,
    {
        let ticket = self.send(context, operation)?;
        self.direct_owners.push((ticket.id(), field));
        crate::Result::Ok(ticket)
    }

    /// Stages one generated call owned by the behavior-tree generation
    /// whose leaf submitted it: the ticket is qualified by this
    /// execution's epoch so no other execution's result can ever claim
    /// it, and the tree-generation ownership record is staged with the
    /// candidate.
    pub fn send_tree<O>(
        &mut self,
        context: &StepContext,
        generation: u64,
        operation: O,
    ) -> crate::Result<CallTicket<O::Response>>
    where
        O: GeneratedSend,
    {
        let ordinal = self.next_ordinal;
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or_else(|| crate::anyhow!("generated runtime operation count overflowed"))?;
        let id = compose_call_ticket(
            context.execution_epoch(),
            context.invocation_index(),
            ordinal,
        )?;
        operation.append_to(self, id)?;
        self.tree_owners.push((id, generation));
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

/// Compile-time bounds for one configured leased output family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputFamily {
    /// Object-valued configuration pointer carrying logical member names.
    pub config_pointer: &'static str,
    /// Literal endpoint suffix.
    pub suffix: &'static str,
    /// Maximum complete membership size.
    pub max_ports: u64,
}

/// Compile-time metadata for one collected output role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputField {
    /// Configuration-owned port family, when this field is a template.
    pub family: Option<OutputFamily>,
    /// Private Rust field or method name.
    pub name: &'static str,
    /// Output role.
    pub kind: OutputKind,
    /// Public port name for served outputs.
    pub port: Option<&'static str>,
    /// Complete generated descriptor identity for a bound served port.
    pub port_signature: Option<MethodSignature>,
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
        _resolve_input_port: &dyn Fn(&str) -> Option<MethodSignature>,
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
        _resolve_input_port: &dyn Fn(&str) -> Option<MethodSignature>,
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
                    let port = *signature;
                    let sequence = wire_sequence(*ticket);
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
                        .map(|output| output.for_instance(*instance).generated_ticket(*ticket))
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
                        .map(|output| output.for_instance(*instance).generated_ticket(*ticket))
                        .map_err(Into::into)
                    }
                }
                GeneratedOperation::Withdraw {
                    instance,
                    signature,
                } => {
                    let ticket = compose_call_ticket(
                        context.execution_epoch(),
                        context.invocation_index(),
                        index,
                    )?;
                    let lease = signature.lease.ok_or_else(|| {
                        crate::anyhow!(
                            "withdrawal {}.{} has no contract lease",
                            signature.service,
                            signature.method,
                        )
                    })?;
                    Ok(super::transport::PreparedOutput::withdrawal(
                        *signature,
                        super::transport::setpoint_metadata(
                            source,
                            context,
                            wire_sequence(ticket),
                            lease.valid_for_ms(),
                        ),
                    )
                    .for_instance(*instance)
                    .generated_ticket(ticket))
                }
            })
            .collect()
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
        _resolve_input_port: &dyn Fn(&str) -> Option<MethodSignature>,
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
