//! The typed Runtime port transport.
//!
//! This module is the process-boundary adapter for generated Protobuf service
//! ports.  The payload is always the generated message itself and carries the
//! standard `application/protobuf` Zenoh encoding.  A small Protobuf
//! attachment carries execution-local delivery facts that are not part of the
//! service payload, such as the logical timestamp and command correlation.
//! Payloads are encoded and decoded through their generated Prost message
//! implementations, so descriptors carry identity only and never own wire
//! functions or a payload registry.

use std::any::{Any, TypeId};
use std::collections::BTreeSet;

use prost::Message;

use super::{ExecutionTime, ObservationStamp, StepContext};
use crate::port::{PortKind, PortSignature};

/// The generated-message capability required by a typed Runtime input.
///
/// Keeping this capability behind `phoxal` means a consuming service can use
/// an imported contract type without taking a second direct dependency on the
/// Prost crate merely because the attribute macro names the bound.
pub trait ProstPayload: Message + prost::Name + Default + Send + Sync + 'static {}

impl<T> ProstPayload for T where T: Message + prost::Name + Default + Send + Sync + 'static {}

/// Encode one generated Prost message using its standard message encoding.
pub fn encode_prost<T: ProstPayload>(value: &T) -> Result<Vec<u8>, prost::EncodeError> {
    let mut bytes = Vec::with_capacity(value.encoded_len());
    value.encode(&mut bytes)?;
    Ok(bytes)
}

/// Decode one generated Prost message using its standard message encoding.
pub fn decode_prost<T: ProstPayload>(bytes: &[u8]) -> Result<T, prost::DecodeError> {
    T::decode(bytes)
}

/// Zenoh's standard encoding identifier for Runtime port payloads.
pub const PROTOBUF_ENCODING: &str = "application/protobuf";

/// Maximum attachment size accepted by one Runtime port sample.
const MAX_METADATA_BYTES: usize = 1024;

/// The semantic control record carried by a stream publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum WireControl {
    /// A normal payload record.
    Data = 0,
    /// A bounded source gap precedes the next retained record.
    Gap = 1,
    /// The source reached a normal terminal state.
    End = 2,
    /// The source reached a terminal failure.
    Failed = 3,
    /// A correlated request was refused before target queue admission.
    Rejected = 4,
}

impl WireControl {
    fn from_raw(value: u32) -> Result<Self, TransportError> {
        match value {
            0 => Ok(Self::Data),
            1 => Ok(Self::Gap),
            2 => Ok(Self::End),
            3 => Ok(Self::Failed),
            4 => Ok(Self::Rejected),
            _ => Err(TransportError::InvalidMetadata {
                detail: format!("unknown stream control value {value}"),
            }),
        }
    }
}

/// Delivery facts attached to one exact generated payload.
///
/// This is metadata, not a second payload envelope.  Decoders hand the body
/// directly to the generated descriptor codec after validating the attachment.
#[derive(Clone, PartialEq, Message)]
pub struct RuntimeWireMetadata {
    /// Per-producer publication sequence, when one exists.
    #[prost(uint64, optional, tag = "1")]
    pub sequence: Option<u64>,
    /// Logical execution time associated with the record.
    #[prost(uint64, optional, tag = "2")]
    pub logical_time_nanos: Option<u64>,
    /// Original source identity for measured/forwarded observations.
    #[prost(string, optional, tag = "3")]
    pub source: Option<String>,
    /// Original observation revision, when the source exposes one.
    #[prost(uint64, optional, tag = "4")]
    pub revision: Option<u64>,
    /// Target command correlation, present for commands and replies.
    #[prost(uint64, optional, tag = "5")]
    pub command_id: Option<u64>,
    /// Deterministic command eligibility boundary.
    #[prost(uint64, optional, tag = "6")]
    pub eligible_boundary: Option<u64>,
    /// Deterministic caller rank used to merge controlled commands.
    #[prost(uint64, optional, tag = "7")]
    pub caller_rank: Option<u64>,
    /// Stream control value.  Zero is ordinary data.
    #[prost(uint32, tag = "8")]
    pub control: u32,
    /// Setpoint expiry in logical execution time, when this is a setpoint
    /// renewal or withdrawal.
    #[prost(uint64, optional, tag = "9")]
    pub expires_at_nanos: Option<u64>,
    /// Stable graph identity of the caller for a Commands request, in the
    /// form `{runtime-instance}.{input-field}`.
    #[prost(string, optional, tag = "10")]
    pub caller: Option<String>,
    /// Target refusal or source failure detail, when supplied.
    #[prost(string, optional, tag = "11")]
    pub reason: Option<String>,
    /// Supervisor ingress sequence for an external command.
    #[prost(uint64, optional, tag = "12")]
    pub ingress_sequence: Option<u64>,
}

impl RuntimeWireMetadata {
    /// Metadata for an ordinary service-produced value.
    #[must_use]
    pub fn data(source: impl Into<String>, logical_time: ExecutionTime, sequence: u64) -> Self {
        Self {
            sequence: Some(sequence),
            logical_time_nanos: Some(logical_time.as_nanos()),
            source: Some(source.into()),
            ..Self::default()
        }
    }

    /// Metadata preserving an input observation's source and capture time.
    #[must_use]
    pub fn observed(stamp: &ObservationStamp, sequence: u64) -> Self {
        Self {
            sequence: Some(sequence),
            logical_time_nanos: Some(stamp.capture_time().as_nanos()),
            source: Some(stamp.source().to_owned()),
            revision: stamp.revision(),
            ..Self::default()
        }
    }

    /// Metadata for a correlated command request or reply.
    #[must_use]
    pub fn command(
        source: impl Into<String>,
        logical_time: ExecutionTime,
        command_id: u64,
        eligible_boundary: u64,
        caller_rank: u64,
    ) -> Self {
        Self {
            sequence: Some(command_id),
            logical_time_nanos: Some(logical_time.as_nanos()),
            source: Some(source.into()),
            command_id: Some(command_id),
            eligible_boundary: Some(eligible_boundary),
            caller_rank: Some(caller_rank),
            ..Self::default()
        }
    }

    /// Metadata for a supervisor-owned external command ingress.
    ///
    /// External commands share the target Commands request key with authored
    /// graph callers, but use a reserved authenticated identity and their own
    /// monotonic ingress sequence.  The target sorts them after every
    /// controlled caller at the same eligible boundary.
    #[must_use]
    pub fn external_request(
        logical_time: ExecutionTime,
        command_id: u64,
        eligible_boundary: u64,
        ingress_sequence: u64,
    ) -> Self {
        Self {
            sequence: Some(command_id),
            logical_time_nanos: Some(logical_time.as_nanos()),
            source: Some("supervisor".to_owned()),
            command_id: Some(command_id),
            eligible_boundary: Some(eligible_boundary),
            caller: Some("supervisor.public".to_owned()),
            ingress_sequence: Some(ingress_sequence),
            ..Self::default()
        }
    }

    /// Metadata for a supervisor-owned external Commands ingress.
    #[must_use]
    pub fn external_command(
        logical_time: ExecutionTime,
        command_id: u64,
        eligible_boundary: u64,
        ingress_sequence: u64,
    ) -> Self {
        Self::external_request(
            logical_time,
            command_id,
            eligible_boundary,
            ingress_sequence,
        )
    }

    pub(crate) fn encode_bounded(&self) -> Result<Vec<u8>, TransportError> {
        let size = self.encoded_len();
        if size > MAX_METADATA_BYTES {
            return Err(TransportError::MetadataTooLarge { bytes: size });
        }
        let mut bytes = Vec::with_capacity(size);
        self.encode(&mut bytes)
            .map_err(|error| TransportError::MetadataEncode(error.to_string()))?;
        Ok(bytes)
    }

    fn decode_bounded(bytes: &[u8]) -> Result<Self, TransportError> {
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(TransportError::MetadataTooLarge { bytes: bytes.len() });
        }
        let metadata = Self::decode(bytes).map_err(|error| TransportError::InvalidMetadata {
            detail: error.to_string(),
        })?;
        WireControl::from_raw(metadata.control)?;
        Ok(metadata)
    }

    /// Returns the logical execution timestamp, if this record carries one.
    pub fn logical_time(&self) -> Result<ExecutionTime, TransportError> {
        self.logical_time_nanos
            .map(ExecutionTime::from_nanos)
            .ok_or_else(|| TransportError::InvalidMetadata {
                detail: "missing logical timestamp".to_owned(),
            })
    }

    /// Returns the stream control record.
    pub fn wire_control(&self) -> Result<WireControl, TransportError> {
        WireControl::from_raw(self.control)
    }

    /// Adds the graph-resolved caller identity to a command request.
    #[must_use]
    pub fn with_caller(mut self, caller: impl Into<String>) -> Self {
        self.caller = Some(caller.into());
        self
    }

    /// Adds an authenticated target refusal reason to correlated metadata.
    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// One received standard-Protobuf Zenoh sample before typed decoding.
#[derive(Clone, Debug)]
pub struct WireSample {
    payload: Vec<u8>,
    metadata: RuntimeWireMetadata,
    key: String,
}

/// Owned descriptor identity admitted from a compiled source bundle.
///
/// Generated consumers do not invent a port name for an unserved input.  The
/// launcher resolves its authored connection to this exact identity and the
/// generated Prost type is checked against the request/response FQNs before a
/// sample can enter an input cut.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortBinding {
    /// Public endpoint name.
    pub name: String,
    /// Owning generated Protobuf service FQN.
    pub service: String,
    /// Protobuf method name.
    pub method: String,
    /// Public endpoint kind.
    pub kind: PortKind,
    /// Fully-qualified request message name.
    pub request: String,
    /// Fully-qualified response message name.
    pub response: String,
}

impl PortBinding {
    /// Copies an inert generated descriptor into an owned launch binding.
    #[must_use]
    pub fn from_signature(signature: PortSignature) -> Self {
        Self {
            name: signature.name.to_owned(),
            service: signature.service.to_owned(),
            method: signature.method.to_owned(),
            kind: signature.kind,
            request: signature.request.to_owned(),
            response: signature.response.to_owned(),
        }
    }
}

impl WireSample {
    #[cfg(test)]
    pub(crate) fn from_parts(
        payload: Vec<u8>,
        metadata: RuntimeWireMetadata,
        key: impl Into<String>,
    ) -> Self {
        Self {
            payload,
            metadata,
            key: key.into(),
        }
    }

    /// Converts one Zenoh sample after validating encoding, attachment, and
    /// bounded metadata.  The payload remains the exact generated message body.
    pub fn from_zenoh(sample: zenoh::sample::Sample) -> Result<Self, TransportError> {
        if sample.encoding().to_string() != PROTOBUF_ENCODING {
            return Err(TransportError::WrongEncoding {
                actual: sample.encoding().to_string(),
            });
        }
        let attachment = sample
            .attachment()
            .ok_or(TransportError::MissingMetadata)?
            .to_bytes();
        let metadata = RuntimeWireMetadata::decode_bounded(&attachment)?;
        Ok(Self {
            payload: sample.payload().to_bytes().into_owned(),
            metadata,
            key: sample.key_expr().as_str().to_owned(),
        })
    }

    /// Returns the exact encoded generated message body.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Returns delivery metadata.
    #[must_use]
    pub const fn metadata(&self) -> &RuntimeWireMetadata {
        &self.metadata
    }

    /// Returns the full key on which the sample arrived.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
}

#[derive(Clone, Debug)]
enum PreparedEndpoint {
    /// A generated descriptor with a compile-time endpoint identity.
    Signature(PortSignature),
    /// A graph-resolved endpoint whose identity came from the launch bundle.
    Binding(PortBinding),
}

/// A type-erased semantic value used by `on_change` output gating.
///
/// The value is cloned and compared through the generated Rust payload type.
/// This intentionally compares message fields, not the bytes produced by a
/// particular Protobuf encoder.
pub struct ChangeToken {
    value: Box<dyn Any + Send + Sync>,
    type_id: TypeId,
    clone_value: fn(&dyn Any) -> Box<dyn Any + Send + Sync>,
    equal_value: fn(&dyn Any, &dyn Any) -> bool,
    stamp: Option<ObservationStamp>,
}

impl ChangeToken {
    /// Captures a semantic payload and optional observation stamp.
    pub fn new<T>(value: &T, stamp: Option<&ObservationStamp>) -> Self
    where
        T: Clone + PartialEq + Send + Sync + 'static,
    {
        Self {
            value: Box::new(value.clone()),
            type_id: TypeId::of::<T>(),
            clone_value: clone_change_value::<T>,
            equal_value: equal_change_value::<T>,
            stamp: stamp.cloned(),
        }
    }
}

#[expect(
    clippy::expect_used,
    reason = "ChangeToken stores the matching TypeId and clone function together"
)]
fn clone_change_value<T>(value: &dyn Any) -> Box<dyn Any + Send + Sync>
where
    T: Clone + Send + Sync + 'static,
{
    Box::new(
        value
            .downcast_ref::<T>()
            .expect("change token type id matches")
            .clone(),
    )
}

fn equal_change_value<T>(left: &dyn Any, right: &dyn Any) -> bool
where
    T: PartialEq + 'static,
{
    left.downcast_ref::<T>() == right.downcast_ref::<T>()
}

impl Clone for ChangeToken {
    fn clone(&self) -> Self {
        Self {
            value: (self.clone_value)(&*self.value),
            type_id: self.type_id,
            clone_value: self.clone_value,
            equal_value: self.equal_value,
            stamp: self.stamp.clone(),
        }
    }
}

impl PartialEq for ChangeToken {
    fn eq(&self, other: &Self) -> bool {
        self.type_id == other.type_id
            && self.stamp == other.stamp
            && (self.equal_value)(&*self.value, &*other.value)
    }
}

impl Eq for ChangeToken {}

impl std::fmt::Debug for ChangeToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChangeToken")
            .field("type_id", &self.type_id)
            .field("stamp", &self.stamp)
            .finish_non_exhaustive()
    }
}

impl PreparedEndpoint {
    fn name(&self) -> &str {
        match self {
            Self::Signature(signature) => signature.name,
            Self::Binding(binding) => &binding.name,
        }
    }
}

/// One encoded output awaiting publication after invocation acceptance.
#[derive(Clone, Debug)]
pub struct PreparedOutput {
    endpoint: PreparedEndpoint,
    target_instance: Option<String>,
    payload: Vec<u8>,
    metadata: RuntimeWireMetadata,
    control: WireControl,
    request: bool,
    reply: bool,
    field: Option<&'static str>,
    change_token: Option<ChangeToken>,
}

impl PreparedOutput {
    /// Encode one ordinary response/publication body with its generated
    /// Protobuf message implementation.
    pub fn response<T: ProstPayload>(
        signature: PortSignature,
        value: &T,
        max_bytes: u64,
        metadata: RuntimeWireMetadata,
    ) -> Result<Self, TransportError> {
        let payload = encode_response(signature, value, max_bytes)?;
        Ok(Self {
            endpoint: PreparedEndpoint::Signature(signature),
            target_instance: None,
            payload,
            metadata,
            control: WireControl::Data,
            request: false,
            reply: false,
            field: None,
            change_token: None,
        })
    }

    /// Encode one correlated command response with its generated Protobuf
    /// message implementation.
    pub fn reply<T: ProstPayload>(
        signature: PortSignature,
        value: &T,
        max_bytes: u64,
        metadata: RuntimeWireMetadata,
    ) -> Result<Self, TransportError> {
        let payload = encode_response(signature, value, max_bytes)?;
        Ok(Self {
            endpoint: PreparedEndpoint::Signature(signature),
            target_instance: None,
            payload,
            metadata,
            control: WireControl::Data,
            request: false,
            reply: true,
            field: None,
            change_token: None,
        })
    }

    /// Encode one managed Read or Request activation body under its declared
    /// request bound.
    pub fn request<T: ProstPayload>(
        signature: PortSignature,
        value: &T,
        max_bytes: u64,
        metadata: RuntimeWireMetadata,
    ) -> Result<Self, TransportError> {
        let payload = encode_response(signature, value, max_bytes)?;
        Ok(Self {
            endpoint: PreparedEndpoint::Signature(signature),
            target_instance: None,
            payload,
            metadata,
            control: WireControl::Data,
            request: true,
            reply: false,
            field: None,
            change_token: None,
        })
    }

    /// Stage an already encoded managed activation body with its resolved
    /// route.  Encoding happens at the generated call site, where the exact
    /// Prost request type is available.
    pub fn request_binding(
        binding: PortBinding,
        payload: Vec<u8>,
        max_bytes: u64,
        metadata: RuntimeWireMetadata,
    ) -> Result<Self, TransportError> {
        if payload.len() as u64 > max_bytes {
            return Err(TransportError::BodyTooLarge {
                port: binding.name.clone(),
                bytes: payload.len(),
                maximum: max_bytes,
            });
        }
        Ok(Self {
            endpoint: PreparedEndpoint::Binding(binding),
            target_instance: None,
            payload,
            metadata,
            control: WireControl::Data,
            request: true,
            reply: false,
            field: None,
            change_token: None,
        })
    }

    /// Create a stream control record with no service payload body.
    pub fn control(
        signature: PortSignature,
        control: WireControl,
        metadata: RuntimeWireMetadata,
    ) -> Self {
        Self {
            endpoint: PreparedEndpoint::Signature(signature),
            target_instance: None,
            payload: Vec::new(),
            metadata,
            control,
            request: false,
            reply: false,
            field: None,
            change_token: None,
        }
    }

    /// Create an explicit setpoint withdrawal without inventing a payload.
    pub fn withdrawal(signature: PortSignature, metadata: RuntimeWireMetadata) -> Self {
        Self {
            endpoint: PreparedEndpoint::Signature(signature),
            target_instance: None,
            payload: Vec::new(),
            metadata,
            control: WireControl::Data,
            request: false,
            reply: false,
            field: None,
            change_token: None,
        }
    }

    /// Associate an encoded record with its generated output method or field.
    #[must_use]
    pub fn for_field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
    }

    /// Attach a typed semantic comparison value for `on_change` gating.
    #[must_use]
    pub fn with_change_token<T>(mut self, value: &T, stamp: Option<&ObservationStamp>) -> Self
    where
        T: Clone + PartialEq + Send + Sync + 'static,
    {
        self.change_token = Some(ChangeToken::new(value, stamp));
        self
    }

    /// Returns the semantic comparison value, when one was attached.
    #[must_use]
    pub fn change_token(&self) -> Option<&ChangeToken> {
        self.change_token.as_ref()
    }

    /// Route a graph-resolved request to its selected producer instance.
    /// Ordinary publications and replies remain local to the publishing
    /// runtime and therefore leave this unset.
    #[must_use]
    pub fn for_instance(mut self, instance: impl Into<String>) -> Self {
        self.target_instance = Some(instance.into());
        self
    }

    /// Returns the generated output field identity, when one was supplied.
    #[must_use]
    pub const fn field(&self) -> Option<&'static str> {
        self.field
    }

    /// Public port signature used to construct the execution-scoped key.
    #[must_use]
    pub fn signature(&self) -> Option<PortSignature> {
        match self.endpoint {
            PreparedEndpoint::Signature(signature) => Some(signature),
            PreparedEndpoint::Binding(_) => None,
        }
    }

    /// Encoded body size used by output batch bounds.
    #[must_use]
    pub const fn payload_len(&self) -> usize {
        self.payload.len()
    }

    fn relative_key(&self, instance: &str) -> String {
        let direction = if self.request {
            "request"
        } else if self.reply {
            "reply"
        } else {
            "publish"
        };
        port_key(instance, self.endpoint.name(), direction)
    }

    fn publish(&self, bus: &crate::bus::BusHandle, instance: &str) -> crate::Result<()> {
        use zenoh::Wait;
        use zenoh::bytes::Encoding;

        let target = self.target_instance.as_deref().unwrap_or(instance);
        let key = bus.full_key(&self.relative_key(target));
        let mut metadata = self.metadata.clone();
        metadata.control = self.control as u32;
        let attachment = metadata.encode_bounded()?;
        bus.session()?
            .put(key, self.payload.clone())
            .encoding(Encoding::from(PROTOBUF_ENCODING.to_owned()))
            .attachment(attachment)
            .wait()
            .map_err(|error| anyhow::anyhow!(TransportError::Transport(error.to_string())))?;
        Ok(())
    }
}

/// Static metadata for one input field with an actual generated descriptor.
#[derive(Clone, Copy, Debug)]
pub struct InputTransportField {
    /// Private input field name.
    pub name: &'static str,
    /// Input semantic form.
    pub kind: crate::runtime::input::InputKind,
    /// Generated public descriptor identity, when the field is explicitly
    /// bound by its Rust declaration.
    pub signature: Option<PortSignature>,
    /// Optional latest-value age bound.
    pub max_age_ms: Option<u64>,
    /// Maximum retained item count, when this form is batched.
    pub max_items: Option<u64>,
    /// Maximum accumulated encoded body bytes, when this form is bounded.
    pub max_bytes: Option<u64>,
}

/// Error from generated input/output transport preparation.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// A sample used an encoding other than standard Protobuf.
    #[error("runtime port sample used encoding `{actual}`, expected `{PROTOBUF_ENCODING}`")]
    WrongEncoding { actual: String },
    /// A sample omitted delivery metadata.
    #[error("runtime port sample is missing its Protobuf delivery metadata")]
    MissingMetadata,
    /// A metadata attachment exceeded its hard bound.
    #[error(
        "runtime port metadata is {bytes} bytes, exceeding the {MAX_METADATA_BYTES}-byte bound"
    )]
    MetadataTooLarge { bytes: usize },
    /// Metadata could not be encoded.
    #[error("runtime port metadata encoding failed: {0}")]
    MetadataEncode(String),
    /// Metadata was malformed or incomplete.
    #[error("invalid runtime port metadata: {detail}")]
    InvalidMetadata { detail: String },
    /// A generated payload could not be encoded as standard Protobuf.
    #[error("runtime port `{port}` payload could not be encoded: {detail}")]
    PayloadEncode { port: String, detail: String },
    /// A generated Prost payload could not be decoded as its declared type.
    #[error("runtime port `{port}` payload could not be decoded: {detail}")]
    PayloadDecode { port: String, detail: String },
    /// One encoded body exceeded its field bound.
    #[error("runtime port `{port}` body is {bytes} bytes, exceeding the {maximum}-byte bound")]
    BodyTooLarge {
        port: String,
        bytes: usize,
        maximum: u64,
    },
    /// One complete output/input batch exceeded a count or byte bound.
    #[error("runtime port `{port}` batch exceeds {what} bound: {actual} > {maximum}")]
    BatchTooLarge {
        port: String,
        what: &'static str,
        actual: u64,
        maximum: u64,
    },
    /// The source sample had an invalid command correlation.
    #[error("runtime command metadata is incomplete: {0}")]
    CommandCorrelation(String),
    /// A Zenoh operation failed.
    #[error("runtime port transport failed: {0}")]
    Transport(String),
}

/// Encode one generated response/publication payload exactly once.
pub fn encode_response<T: ProstPayload>(
    signature: PortSignature,
    value: &T,
    max_bytes: u64,
) -> Result<Vec<u8>, TransportError> {
    let payload = encode_prost(value).map_err(|error| TransportError::PayloadEncode {
        port: signature.name.to_owned(),
        detail: error.to_string(),
    })?;
    if payload.len() as u64 > max_bytes {
        return Err(TransportError::BodyTooLarge {
            port: signature.name.to_owned(),
            bytes: payload.len(),
            maximum: max_bytes,
        });
    }
    Ok(payload)
}

/// Decode one generated command request into the exact Rust request type.
pub fn decode_request<T: ProstPayload>(
    signature: PortSignature,
    sample: &WireSample,
    max_bytes: u64,
) -> Result<T, TransportError> {
    if sample.payload().len() as u64 > max_bytes {
        return Err(TransportError::BodyTooLarge {
            port: signature.name.to_owned(),
            bytes: sample.payload().len(),
            maximum: max_bytes,
        });
    }
    if sample.metadata().wire_control()? != WireControl::Data {
        return Err(TransportError::InvalidMetadata {
            detail: "command request used a stream control record".to_owned(),
        });
    }
    T::decode(sample.payload()).map_err(|error| TransportError::PayloadDecode {
        port: signature.name.to_owned(),
        detail: error.to_string(),
    })
}

/// Decode one generated Prost message body after checking the exact source
/// descriptor and bounded body size.
pub fn decode_message<T>(
    binding: &PortBinding,
    sample: &WireSample,
    max_bytes: u64,
) -> Result<T, TransportError>
where
    T: ProstPayload,
{
    if sample.payload().len() as u64 > max_bytes {
        return Err(TransportError::BodyTooLarge {
            port: binding.name.clone(),
            bytes: sample.payload().len(),
            maximum: max_bytes,
        });
    }
    if sample.metadata().wire_control()? != WireControl::Data {
        return Err(TransportError::InvalidMetadata {
            detail: format!(
                "port `{}` carried a control record where data was required",
                binding.name
            ),
        });
    }
    T::decode(sample.payload()).map_err(|error| TransportError::PayloadDecode {
        port: binding.name.clone(),
        detail: error.to_string(),
    })
}

/// Validate a publication binding against one generated Prost payload type.
pub fn validate_publication_binding<T>(
    binding: &PortBinding,
    expected_kind: PortKind,
) -> Result<(), TransportError>
where
    T: ProstPayload,
{
    let response = T::full_name();
    validate_binding(binding, expected_kind, "google.protobuf.Empty", &response)
}

/// Validate a request/response binding against generated Prost payload types.
pub fn validate_exchange_binding<Request, Response>(
    binding: &PortBinding,
    expected_kind: PortKind,
) -> Result<(), TransportError>
where
    Request: ProstPayload,
    Response: ProstPayload,
{
    let request = Request::full_name();
    let response = Response::full_name();
    validate_binding(binding, expected_kind, &request, &response)
}

/// Validate that an input route retained the exact generated descriptor that
/// its local Commands field explicitly selected.
pub fn validate_binding_identity(
    binding: &PortBinding,
    expected: PortSignature,
) -> Result<(), TransportError> {
    let expected = PortBinding::from_signature(expected);
    if binding != &expected {
        return Err(TransportError::InvalidMetadata {
            detail: format!(
                "port `{}` descriptor does not match the generated local binding",
                binding.name
            ),
        });
    }
    Ok(())
}

fn validate_binding(
    binding: &PortBinding,
    expected_kind: PortKind,
    expected_request: &str,
    expected_response: &str,
) -> Result<(), TransportError> {
    if binding.kind != expected_kind
        || binding.request != expected_request
        || binding.response != expected_response
    {
        return Err(TransportError::InvalidMetadata {
            detail: format!(
                "port `{}` descriptor mismatch: expected kind `{}`, request `{expected_request}`, response `{expected_response}`, got kind `{}`, request `{}`, response `{}`",
                binding.name,
                expected_kind.as_str(),
                binding.kind.as_str(),
                binding.request,
                binding.response,
            ),
        });
    }
    Ok(())
}

/// Sort a bounded Commands cut by the source-independent admission key.
///
/// Zenoh preserves one publisher's FIFO order, but a connected service can
/// receive concurrent callers in a different order.  The attachment is the
/// authoritative merge key, so queue arrival order is never allowed to alter
/// behavior.  Correlation ids are also unique within one target queue; a
/// duplicate is rejected before any service step can observe it.
pub fn sort_command_samples(samples: &mut [WireSample]) -> Result<(), TransportError> {
    let mut identities = BTreeSet::new();
    for sample in samples.iter() {
        if sample.metadata.wire_control()? != WireControl::Data {
            return Err(TransportError::CommandCorrelation(
                "command request used a stream control record".to_owned(),
            ));
        }
        let metadata = &sample.metadata;
        if metadata.command_id.is_none()
            || metadata.eligible_boundary.is_none()
            || metadata.source.as_deref().is_none_or(str::is_empty)
            || metadata.caller.as_deref().is_none_or(str::is_empty)
        {
            return Err(TransportError::CommandCorrelation(
                "command request is missing source, caller, command_id, eligible_boundary, or caller_rank"
                    .to_owned(),
            ));
        }
        let _ = command_ingress(metadata)?;
        // The caller/source identity and command id are the stable replay key.
        // The rank is validated separately and is deliberately not part of
        // identity, so a malformed replay cannot evade duplicate detection by
        // changing its carried ordering metadata.
        let identity = (
            metadata.source.clone().unwrap_or_default(),
            metadata.caller.clone().unwrap_or_default(),
            metadata.command_id.unwrap_or_default(),
        );
        if !identities.insert(identity) {
            return Err(TransportError::CommandCorrelation(format!(
                "duplicate command id {} from caller `{}` in one input cut",
                metadata.command_id.unwrap_or_default(),
                metadata.caller.as_deref().unwrap_or_default(),
            )));
        }
    }
    samples.sort_by_key(|sample| {
        let metadata = &sample.metadata;
        let ingress = command_ingress(metadata).unwrap_or({
            // Every sample was validated immediately above.  Invalid records
            // are sorted after valid records defensively if this invariant is
            // ever changed without updating that validation.
            CommandIngress::External {
                ingress_sequence: u64::MAX,
            }
        });
        let (category, order, tie_breaker) = match ingress {
            CommandIngress::Controlled { caller_rank } => (0_u8, caller_rank, 0_u64),
            CommandIngress::External { ingress_sequence } => (1_u8, 0_u64, ingress_sequence),
        };
        (
            metadata.eligible_boundary.unwrap_or_default(),
            category,
            order,
            tie_breaker,
            metadata.command_id.unwrap_or_default(),
        )
    });
    Ok(())
}

/// The two authenticated command-ingress categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandIngress {
    /// A caller selected by the compiled authored graph.
    Controlled { caller_rank: u64 },
    /// A supervisor-owned public ingress with a target-local sequence.
    External { ingress_sequence: u64 },
}

/// Validate and classify command metadata before queue admission.
pub fn command_ingress(metadata: &RuntimeWireMetadata) -> Result<CommandIngress, TransportError> {
    let source = metadata.source.as_deref().unwrap_or_default();
    let caller = metadata.caller.as_deref().unwrap_or_default();
    let reserved_source = source == "supervisor";
    let reserved_caller = caller == "supervisor.public";
    if reserved_source || reserved_caller {
        if !(reserved_source && reserved_caller) {
            return Err(TransportError::CommandCorrelation(
                "external command must use source `supervisor` and caller `supervisor.public`"
                    .to_owned(),
            ));
        }
        if metadata.caller_rank.is_some() {
            return Err(TransportError::CommandCorrelation(
                "external command must not carry caller_rank".to_owned(),
            ));
        }
        let ingress_sequence = metadata.ingress_sequence.ok_or_else(|| {
            TransportError::CommandCorrelation(
                "external command is missing ingress_sequence".to_owned(),
            )
        })?;
        return Ok(CommandIngress::External { ingress_sequence });
    }
    if metadata.ingress_sequence.is_some() {
        return Err(TransportError::CommandCorrelation(
            "ingress_sequence is reserved for supervisor external commands".to_owned(),
        ));
    }
    let caller_rank = metadata.caller_rank.ok_or_else(|| {
        TransportError::CommandCorrelation("controlled command is missing caller_rank".to_owned())
    })?;
    Ok(CommandIngress::Controlled { caller_rank })
}

/// Convert validated command metadata into the public input ordering key.
pub fn command_order(
    metadata: &RuntimeWireMetadata,
) -> Result<super::input::CommandOrder, TransportError> {
    let eligible_boundary = metadata.eligible_boundary.ok_or_else(|| {
        TransportError::CommandCorrelation("missing eligible_boundary".to_owned())
    })?;
    let command_id = metadata
        .command_id
        .ok_or_else(|| TransportError::CommandCorrelation("missing command_id".to_owned()))?;
    let sequence = super::input::CommandId::new(command_id);
    match command_ingress(metadata)? {
        CommandIngress::Controlled { caller_rank } => Ok(super::input::CommandOrder::new(
            eligible_boundary,
            caller_rank,
            sequence,
        )),
        CommandIngress::External { ingress_sequence } => Ok(super::input::CommandOrder::external(
            eligible_boundary,
            ingress_sequence,
            sequence,
        )),
    }
}

/// Returns an execution-scoped relative key for one Runtime port direction.
pub fn port_key(instance: &str, port: &str, direction: &str) -> String {
    format!("runtime/{instance}/ports/{port}/{direction}")
}

/// Returns the observation stamp represented by one data sample's metadata.
pub fn observation_stamp(
    metadata: &RuntimeWireMetadata,
) -> Result<ObservationStamp, TransportError> {
    let logical_time = metadata.logical_time()?;
    let source = metadata
        .source
        .clone()
        .ok_or_else(|| TransportError::InvalidMetadata {
            detail: "missing observation source".to_owned(),
        })?;
    Ok(ObservationStamp::new(
        source,
        logical_time,
        metadata.revision,
    ))
}

/// Resolve the generated port bound to one Commands input field.
pub fn input_port_signature<S: crate::runtime::input::InputSet>(
    field: &str,
) -> Option<PortSignature> {
    S::FIELDS
        .iter()
        .find(|candidate| candidate.name == field)
        .and_then(|candidate| candidate.port_signature)
}

/// Validate and sum one output batch against count/byte bounds.
pub fn check_batch(
    signature: PortSignature,
    count: usize,
    bytes: usize,
    max_items: u64,
    max_bytes: u64,
) -> Result<(), TransportError> {
    if count as u64 > max_items {
        return Err(TransportError::BatchTooLarge {
            port: signature.name.to_owned(),
            what: "item count",
            actual: count as u64,
            maximum: max_items,
        });
    }
    if bytes as u64 > max_bytes {
        return Err(TransportError::BatchTooLarge {
            port: signature.name.to_owned(),
            what: "encoded bytes",
            actual: bytes as u64,
            maximum: max_bytes,
        });
    }
    Ok(())
}

/// Add one encoded item to a bounded batch without allowing arithmetic
/// overflow or a temporary over-capacity reservation.
pub fn checked_add_batch_bytes(
    signature: PortSignature,
    current: usize,
    additional: usize,
    maximum: u64,
) -> Result<usize, TransportError> {
    let actual = current
        .checked_add(additional)
        .ok_or_else(|| TransportError::BatchTooLarge {
            port: signature.name.to_owned(),
            what: "encoded bytes",
            actual: u64::MAX,
            maximum,
        })?;
    if actual as u64 > maximum {
        return Err(TransportError::BatchTooLarge {
            port: signature.name.to_owned(),
            what: "encoded bytes",
            actual: actual as u64,
            maximum,
        });
    }
    Ok(actual)
}

/// Publish one complete already-reserved output batch.
pub fn publish_batch(
    bus: &crate::bus::BusHandle,
    instance: &str,
    outputs: &[PreparedOutput],
) -> crate::Result<()> {
    for output in outputs {
        output.publish(bus, instance)?;
    }
    Ok(())
}

/// Build ordinary metadata for a publication callback.
#[must_use]
pub fn publication_metadata(
    source: &str,
    context: StepContext,
    sequence: u64,
) -> RuntimeWireMetadata {
    RuntimeWireMetadata::data(source.to_owned(), context.now(), sequence)
}

/// Build metadata for a sampled forwarded observation.
#[must_use]
pub fn sample_metadata(stamp: &ObservationStamp, sequence: u64) -> RuntimeWireMetadata {
    RuntimeWireMetadata::observed(stamp, sequence)
}

/// Build metadata for an accepted setpoint renewal or withdrawal.
#[must_use]
pub fn setpoint_metadata(
    source: &str,
    context: StepContext,
    sequence: u64,
    valid_for_ms: u64,
) -> RuntimeWireMetadata {
    let mut metadata = RuntimeWireMetadata::data(source.to_owned(), context.now(), sequence);
    metadata.expires_at_nanos = context
        .now()
        .as_nanos()
        .checked_add(valid_for_ms.saturating_mul(1_000_000));
    metadata
}

/// Build metadata for a correlated reply.
#[must_use]
pub fn reply_metadata(
    source: &str,
    context: StepContext,
    command_id: u64,
    eligible_boundary: u64,
    caller_rank: u64,
) -> RuntimeWireMetadata {
    RuntimeWireMetadata::command(
        source.to_owned(),
        context.now(),
        command_id,
        eligible_boundary,
        caller_rank,
    )
}

/// Build metadata for a reply while retaining supervisor external ingress
/// identity when the originating command came through the public boundary.
#[must_use]
pub fn reply_metadata_for_order(
    source: &str,
    context: StepContext,
    order: super::input::CommandOrder,
) -> RuntimeWireMetadata {
    let mut metadata = reply_metadata(
        source,
        context,
        order.sequence().sequence(),
        order.eligible_boundary(),
        order.caller_rank(),
    );
    if let Some(ingress_sequence) = order.ingress_sequence() {
        metadata.caller = Some("supervisor.public".to_owned());
        metadata.ingress_sequence = Some(ingress_sequence);
        metadata.caller_rank = None;
    }
    metadata
}

/// Build metadata for a Read reply while retaining the caller's ingress
/// category and sequence.  Public Read requests use the same authenticated
/// supervisor ticket as external Commands, but do not participate in command
/// ordering at a target invocation boundary.
pub fn reply_metadata_for_request(
    source: &str,
    context: StepContext,
    request: &RuntimeWireMetadata,
) -> Result<RuntimeWireMetadata, TransportError> {
    let command_id = request
        .command_id
        .ok_or_else(|| TransportError::CommandCorrelation("missing command_id".to_owned()))?;
    let eligible_boundary = request.eligible_boundary.ok_or_else(|| {
        TransportError::CommandCorrelation("missing eligible_boundary".to_owned())
    })?;
    let ingress = command_ingress(request)?;
    let mut reply = reply_metadata(
        source,
        context,
        command_id,
        eligible_boundary,
        match ingress {
            CommandIngress::Controlled { caller_rank } => caller_rank,
            CommandIngress::External { .. } => 0,
        },
    );
    if let CommandIngress::External { ingress_sequence } = ingress {
        reply.caller = Some("supervisor.public".to_owned());
        reply.ingress_sequence = Some(ingress_sequence);
        reply.caller_rank = None;
    }
    Ok(reply)
}

/// Build metadata for a graph-resolved request activation.  The caller
/// identity is carried separately from the source instance so a target can
/// validate a canonical rank even when one source has multiple request fields
/// connected to the same Commands port.
#[must_use]
pub fn request_metadata(
    source: &str,
    caller: &str,
    context: StepContext,
    command_id: u64,
    eligible_boundary: u64,
    caller_rank: u64,
) -> RuntimeWireMetadata {
    RuntimeWireMetadata::command(
        source.to_owned(),
        context.now(),
        command_id,
        eligible_boundary,
        caller_rank,
    )
    .with_caller(caller)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(metadata: RuntimeWireMetadata) -> WireSample {
        WireSample {
            payload: Vec::new(),
            metadata,
            key: "runtime/target/ports/commands/request".to_owned(),
        }
    }

    #[test]
    fn external_ingress_is_authenticated_and_sorted_after_controlled_callers() {
        let external = RuntimeWireMetadata::external_command(ExecutionTime::default(), 20, 7, 3);
        let controlled = RuntimeWireMetadata::command("caller", ExecutionTime::default(), 19, 7, 4)
            .with_caller("caller.request");
        let earlier_controlled =
            RuntimeWireMetadata::command("caller-a", ExecutionTime::default(), 18, 7, 1)
                .with_caller("caller-a.request");
        let mut samples = vec![
            sample(external),
            sample(controlled),
            sample(earlier_controlled),
        ];

        sort_command_samples(&mut samples).expect("command metadata is valid");
        assert_eq!(
            samples[0].metadata.caller.as_deref(),
            Some("caller-a.request")
        );
        assert_eq!(
            samples[1].metadata.caller.as_deref(),
            Some("caller.request")
        );
        assert_eq!(
            samples[2].metadata.caller.as_deref(),
            Some("supervisor.public")
        );
        assert_eq!(
            command_order(samples[2].metadata()).expect("external order"),
            super::super::input::CommandOrder::external(
                7,
                3,
                super::super::input::CommandId::new(20),
            )
        );
    }

    #[test]
    fn external_ingress_rejects_missing_or_mixed_reserved_identity() {
        let mut missing_sequence =
            RuntimeWireMetadata::external_command(ExecutionTime::default(), 1, 0, 2);
        missing_sequence.ingress_sequence = None;
        assert!(command_ingress(&missing_sequence).is_err());

        let mixed = RuntimeWireMetadata::command("supervisor", ExecutionTime::default(), 1, 0, 0)
            .with_caller("supervisor.public");
        assert!(command_ingress(&mixed).is_err());

        let controlled_with_external_sequence =
            RuntimeWireMetadata::command("caller", ExecutionTime::default(), 1, 0, 0)
                .with_caller("caller.request");
        let mut controlled_with_external_sequence = controlled_with_external_sequence;
        controlled_with_external_sequence.ingress_sequence = Some(1);
        assert!(command_ingress(&controlled_with_external_sequence).is_err());
    }
}
