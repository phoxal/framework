//! The typed Runtime port transport.
//!
//! This module is the process-boundary adapter for generated Protobuf service
//! ports.  The payload is always the generated message itself and carries the
//! standard `application/protobuf` Zenoh encoding.  A small Protobuf
//! attachment carries execution-local delivery facts that are not part of the
//! service payload, such as the logical timestamp and command correlation.
//! Descriptors provide the erased codec functions, so this module never keeps a
//! handwritten payload catalogue or falls back to the legacy MessagePack bus.

use std::any::{Any, TypeId};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

use prost::Message;

use super::{ExecutionTime, ObservationStamp, StepContext};
use crate::port::{CodecError, PortCodec, PortKind, PortSignature};

/// The generated-message capability required by a typed Runtime input.
///
/// Keeping this capability behind `phoxal` means a consuming service can use
/// an imported contract type without taking a second direct dependency on the
/// Prost crate merely because the attribute macro names the bound.
pub trait ProstPayload: Message + prost::Name + Default + Send + Sync + 'static {}

impl<T> ProstPayload for T where T: Message + prost::Name + Default + Send + Sync + 'static {}

static REGISTERED_CODECS: OnceLock<Mutex<HashMap<TypeId, PortCodec>>> = OnceLock::new();

fn codec_registry() -> &'static Mutex<HashMap<TypeId, PortCodec>> {
    REGISTERED_CODECS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register the erased request and response functions carried by one
/// generated descriptor.  The generic types are used only as stable `TypeId`
/// keys, so direct non-Prost fixtures remain compilable and simply have no
/// runtime codec until a generated descriptor is registered.
pub fn register_exchange_codecs<Request: 'static, Response: 'static>(signature: PortSignature) {
    let Some(codec) = signature.codec() else {
        return;
    };
    let mut registry = match codec_registry().lock() {
        Ok(registry) => registry,
        Err(poisoned) => poisoned.into_inner(),
    };
    registry.insert(TypeId::of::<Request>(), codec.request_only());
    registry.insert(TypeId::of::<Response>(), codec.response_only());
}

/// Register one generated publication response codec.
pub fn register_response_codec<Response: 'static>(signature: PortSignature) {
    let Some(codec) = signature.codec() else {
        return;
    };
    let mut registry = match codec_registry().lock() {
        Ok(registry) => registry,
        Err(poisoned) => poisoned.into_inner(),
    };
    registry.insert(TypeId::of::<Response>(), codec.response_only());
}

/// Look up a generated request codec for one erased activation body.
#[must_use]
pub fn registered_codec<T: 'static>() -> Option<PortCodec> {
    let registry = match codec_registry().lock() {
        Ok(registry) => registry,
        Err(poisoned) => poisoned.into_inner(),
    };
    registry.get(&TypeId::of::<T>()).copied()
}

/// Encode one generated Prost message through the erased descriptor codec
/// shape used by [`PortSignature`].
pub fn encode_prost<T: ProstPayload>(value: &dyn Any) -> Result<Vec<u8>, CodecError> {
    let value = value.downcast_ref::<T>().ok_or(CodecError::TypeMismatch)?;
    let mut bytes = Vec::with_capacity(value.encoded_len());
    value.encode(&mut bytes).map_err(|_| CodecError::Encode)?;
    Ok(bytes)
}

/// Decode one generated Prost message through the erased descriptor codec
/// shape used by [`PortSignature`].
pub fn decode_prost<T: ProstPayload>(
    bytes: &[u8],
) -> Result<Box<dyn Any + Send + Sync>, CodecError> {
    T::decode(bytes)
        .map(|value| Box::new(value) as Box<dyn Any + Send + Sync>)
        .map_err(|_| CodecError::Decode)
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
}

impl WireControl {
    fn from_raw(value: u32) -> Result<Self, TransportError> {
        match value {
            0 => Ok(Self::Data),
            1 => Ok(Self::Gap),
            2 => Ok(Self::End),
            3 => Ok(Self::Failed),
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
}

impl PreparedOutput {
    /// Encode one ordinary response/publication body under its declared bound.
    pub fn response(
        signature: PortSignature,
        value: &dyn Any,
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
        })
    }

    /// Encode one correlated command response under its declared bound.
    pub fn reply(
        signature: PortSignature,
        value: &dyn Any,
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
        })
    }

    /// Encode one managed Read or Request activation body under its declared
    /// request bound.  The generated input descriptor supplies the exact
    /// request codec, so no endpoint-name registry is consulted here.
    pub fn request(
        signature: PortSignature,
        value: &dyn Any,
        max_bytes: u64,
        metadata: RuntimeWireMetadata,
    ) -> Result<Self, TransportError> {
        let codec = signature.codec().ok_or(TransportError::MissingCodec {
            port: signature.name.to_owned(),
            direction: "request",
        })?;
        let payload = codec
            .encode_request(value)
            .map_err(|source| TransportError::Codec {
                port: signature.name.to_owned(),
                source,
            })?;
        if payload.len() as u64 > max_bytes {
            return Err(TransportError::BodyTooLarge {
                port: signature.name.to_owned(),
                bytes: payload.len(),
                maximum: max_bytes,
            });
        }
        Ok(Self {
            endpoint: PreparedEndpoint::Signature(signature),
            target_instance: None,
            payload,
            metadata,
            control: WireControl::Data,
            request: true,
            reply: false,
            field: None,
        })
    }

    /// Encode one graph-resolved managed activation body with the generated
    /// request codec belonging to the local input field.
    pub fn request_binding(
        binding: PortBinding,
        codec: PortCodec,
        value: &dyn Any,
        max_bytes: u64,
        metadata: RuntimeWireMetadata,
    ) -> Result<Self, TransportError> {
        let payload = codec
            .encode_request(value)
            .map_err(|source| TransportError::Codec {
                port: binding.name.clone(),
                source,
            })?;
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
        }
    }

    /// Associate an encoded record with its generated output method or field.
    #[must_use]
    pub fn for_field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
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
    /// Generated request codec for Read/Request activation bodies.
    pub request_codec: Option<PortCodec>,
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
    /// The generated descriptor does not carry the requested codec.
    #[error("generated port `{port}` has no {direction} Protobuf codec")]
    MissingCodec {
        port: String,
        direction: &'static str,
    },
    /// The generated codec received a value of an unexpected Rust type.
    #[error("generated port `{port}` codec rejected the Rust value: {source}")]
    Codec {
        port: String,
        #[source]
        source: CodecError,
    },
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
pub fn encode_response(
    signature: PortSignature,
    value: &dyn Any,
    max_bytes: u64,
) -> Result<Vec<u8>, TransportError> {
    let codec = signature.codec().ok_or(TransportError::MissingCodec {
        port: signature.name.to_owned(),
        direction: "response",
    })?;
    let payload = codec
        .encode_response(value)
        .map_err(|source| TransportError::Codec {
            port: signature.name.to_owned(),
            source,
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
pub fn decode_request<T: Send + Sync + 'static>(
    signature: PortSignature,
    sample: &WireSample,
) -> Result<T, TransportError> {
    let value = decode_request_value(signature, sample, u64::MAX)?;
    value
        .downcast::<T>()
        .map(|value| *value)
        .map_err(|_| TransportError::Codec {
            port: signature.name.to_owned(),
            source: CodecError::TypeMismatch,
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

/// Decode a generated request body into an erased Send value.  The caller
/// performs the exact Rust downcast after checking the source descriptor, so
/// direct runtime fixtures can retain private request types while generated
/// service ports use their Prost codecs.
pub fn decode_request_value(
    signature: PortSignature,
    sample: &WireSample,
    max_bytes: u64,
) -> Result<super::input::TransportValue, TransportError> {
    if sample.payload().len() as u64 > max_bytes {
        return Err(TransportError::BodyTooLarge {
            port: signature.name.to_owned(),
            bytes: sample.payload().len(),
            maximum: max_bytes,
        });
    }
    if sample.metadata.wire_control()? != WireControl::Data {
        return Err(TransportError::InvalidMetadata {
            detail: "command request used a stream control record".to_owned(),
        });
    }
    let codec = signature.codec().ok_or(TransportError::MissingCodec {
        port: signature.name.to_owned(),
        direction: "request",
    })?;
    let value = codec
        .decode_request(sample.payload())
        .map_err(|source| TransportError::Codec {
            port: signature.name.to_owned(),
            source,
        })?;
    Ok(value)
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
            || metadata.caller_rank.is_none()
            || metadata.source.as_deref().is_none_or(str::is_empty)
            || metadata.caller.as_deref().is_none_or(str::is_empty)
        {
            return Err(TransportError::CommandCorrelation(
                "command request is missing source, caller, command_id, eligible_boundary, or caller_rank"
                    .to_owned(),
            ));
        }
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
        (
            metadata.eligible_boundary.unwrap_or_default(),
            metadata.caller_rank.unwrap_or_default(),
            metadata.command_id.unwrap_or_default(),
        )
    });
    Ok(())
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

/// Return the generated response codec for a port, useful to reply encoders.
#[must_use]
pub const fn descriptor_codec(signature: PortSignature) -> Option<PortCodec> {
    signature.codec()
}
