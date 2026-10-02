//! Inert generated service methods and robot-instance operations.
//!
//! Protobuf cardinality determines whether a method is a one-shot call or an
//! observation source. The generated robot facade binds these contract-owned
//! method descriptors to one exact `robot.yaml` service instance. Constructing
//! a value in this module performs no I/O and grants no authority.

use std::fmt;
use std::marker::PhantomData;

/// Field wrapper marking a latest-delivery endpoint in an API declaration.
///
/// The value is never constructed; `#[phoxal::endpoints]` reads it as the
/// endpoint's delivery shape and generates the runtime form.
pub struct Latest<T>(PhantomData<fn() -> T>);

/// Field wrapper marking a queued-delivery endpoint in an API declaration.
pub struct Queue<T>(PhantomData<fn() -> T>);

/// Field wrapper marking a retained state publication in an API declaration.
///
/// A `State<T>` output publishes its pure `#[publish]` value after successful
/// initialization and after each accepted invocation; initial publication is
/// part of the endpoint semantic, so no bootstrap flag is declared.
pub struct State<T>(PhantomData<fn() -> T>);

/// Field wrapper marking a request/response endpoint in an API declaration.
pub struct RequestReply<Request, Response>(PhantomData<fn(Request) -> Response>);

/// One typed outgoing operation descriptor: the canonical identity of an
/// operation together with the request and response payloads it exchanges.
///
/// The same protocol serves generated provider descriptors, SDK-owned
/// operations, and a package's privately authored expectations, so one call
/// field spelling `#[phoxal::call] name: Desc` works regardless of where the
/// descriptor is defined. A descriptor is inert metadata: it is not an
/// instance handle and does not submit a request.
pub trait Operation: Sized + Send + Sync + 'static {
    /// The request payload submitted to the operation.
    type Request: ProstPayload;
    /// The response payload returned for one accepted request.
    type Response: ProstPayload;
    /// The complete contract-owned method identity of this operation.
    ///
    /// Its shape must be [`MethodShape::Call`]; the descriptor trait carries
    /// a shape check that the endpoint expansion instantiates.
    const METHOD: CallMethod<Self::Request, Self::Response>;

    /// The canonical contract-owned method identity.
    const SIGNATURE: MethodSignature = Self::METHOD.signature();

    /// Compiles only when the descriptor names a unary call method.
    const CALL_SHAPE: () = assert!(
        matches!(Self::SIGNATURE.shape, MethodShape::Call),
        "an Operation descriptor must carry a unary call identity",
    );

    /// One inert call to this operation, bound to one service instance.
    fn bind(instance: &'static str, request: Self::Request) -> Call<Self::Request, Self::Response> {
        Self::METHOD.bind(instance, request)
    }
}

/// Canonical generated representation of `google.protobuf.Empty`.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct Empty {}

impl prost::Name for Empty {
    const NAME: &'static str = "Empty";
    const PACKAGE: &'static str = "google.protobuf";

    fn full_name() -> prost::alloc::string::String {
        "google.protobuf.Empty".into()
    }

    fn type_url() -> prost::alloc::string::String {
        "/google.protobuf.Empty".into()
    }
}

impl crate::schema::MessageSchema for Empty {
    const RECORD: crate::schema::SchemaRecord<'static> =
        crate::schema::SchemaRecord::Message(crate::schema::MessageRecord {
            package: "google.protobuf",
            name: "Empty",
            fields: &[],
        });
    const WIRE_NAME: &'static str = "google.protobuf.Empty";

    fn retain_schema() -> usize {
        0
    }
}

/// Public method shape derived from Protobuf cardinality.
/// The transport codec capability of one endpoint payload type.
///
/// Ordinary messages are their own wire form. A payload enum — a Rust
/// enum whose variants carry explicit stable tags — has no valid
/// unselected value to default to, so it decodes through a private Prost
/// wire mirror and [`ProstPayload::try_from_wire`] fails when the wire
/// form selects no variant. Endpoints, the runtime transport, and public
/// sessions all move payload bytes through this trait instead of
/// requiring `Default` or `prost::Message` on the public type; wire
/// identity comes from [`crate::schema::MessageSchema::WIRE_NAME`]
/// because `prost::Name` requires `prost::Message`.
pub trait ProstPayload: Sized + crate::schema::MessageSchema + Send + Sync + 'static {
    /// The Prost wire form carrying this payload on transports.
    type Wire: prost::Message + Default + Send + Sync + 'static;
    /// The wire form of this value.
    fn to_wire(&self) -> Self::Wire;
    /// Rebuilds the payload from its wire form, failing when the wire
    /// form carries no valid value.
    fn try_from_wire(wire: Self::Wire) -> Result<Self, prost::DecodeError>;

    /// Encodes this payload's wire form to bytes.
    fn encode_payload(&self) -> Result<Vec<u8>, prost::EncodeError> {
        use prost::Message as _;
        let wire = self.to_wire();
        let mut bytes = Vec::with_capacity(wire.encoded_len());
        wire.encode(&mut bytes)?;
        Ok(bytes)
    }

    /// Decodes this payload from its wire form's bytes.
    fn decode_payload(bytes: &[u8]) -> Result<Self, prost::DecodeError> {
        Self::try_from_wire(<Self::Wire as prost::Message>::decode(bytes)?)
    }
}

/// Implements [`ProstPayload`] for a message that is its own wire form.
macro_rules! wire_payload {
    ($($ty:ty),* $(,)?) => {
        $(
            impl ProstPayload for $ty {
                type Wire = Self;

                fn to_wire(&self) -> Self {
                    <Self as Clone>::clone(self)
                }

                fn try_from_wire(wire: Self) -> Result<Self, prost::DecodeError> {
                    Ok(wire)
                }
            }
        )*
    };
}

wire_payload!(Empty);

/// Public method shape derived from Protobuf cardinality.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MethodShape {
    /// One unary request and one unary response.
    Call,
    /// An empty request and a server-streamed value.
    Observation,
}

/// Lease behavior declared by a method contract.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Lease {
    valid_for_ms: u64,
}

impl Lease {
    /// Creates a positive lease interval.
    #[must_use]
    pub const fn new(valid_for_ms: u64) -> Self {
        assert!(valid_for_ms > 0, "lease validity must be positive");
        Self { valid_for_ms }
    }

    /// Returns the contract-owned validity interval.
    #[must_use]
    pub const fn valid_for_ms(self) -> u64 {
        self.valid_for_ms
    }
}

/// Complete generated identity and behavior for one service method.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MethodSignature {
    /// Fully-qualified Protobuf service name.
    pub service: &'static str,
    /// Protobuf method name.
    pub method: &'static str,
    /// Stable snake-case endpoint name used by execution adapters.
    pub endpoint: &'static str,
    /// Cardinality-derived method shape.
    pub shape: MethodShape,
    /// Fully-qualified request message name.
    pub request: &'static str,
    /// Fully-qualified response or observation message name.
    pub response: &'static str,
    /// Whether admission replays the latest accepted observation.
    pub retained_latest: bool,
    /// Optional replaceable authority interval.
    pub lease: Option<Lease>,
    descriptor_set: &'static [u8],
}

impl MethodSignature {
    /// Creates generated method metadata retaining the owner's descriptor closure.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        service: &'static str,
        method: &'static str,
        endpoint: &'static str,
        shape: MethodShape,
        request: &'static str,
        response: &'static str,
        retained_latest: bool,
        lease_valid_for_ms: Option<u64>,
        descriptor_set: &'static [u8],
    ) -> Self {
        Self {
            service,
            method,
            endpoint,
            shape,
            request,
            response,
            retained_latest,
            lease: match lease_valid_for_ms {
                Some(value) => Some(Lease::new(value)),
                None => None,
            },
            descriptor_set,
        }
    }

    /// Returns the exact retained descriptor closure.
    #[must_use]
    pub const fn descriptor_set(self) -> &'static [u8] {
        self.descriptor_set
    }
}

/// Shared behavior exposed by every generated method descriptor.
pub trait MethodDescriptor: Copy + fmt::Debug + Send + Sync + 'static {
    /// Returns the complete contract-owned method identity.
    fn signature(self) -> MethodSignature;
}

/// Contract-owned descriptor for a unary call.
pub struct CallMethod<Request, Response> {
    signature: MethodSignature,
    marker: PhantomData<fn(Request) -> Response>,
}

impl<Request, Response> CallMethod<Request, Response> {
    /// Creates one generated unary method descriptor.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        service: &'static str,
        method: &'static str,
        endpoint: &'static str,
        request: &'static str,
        response: &'static str,
        lease_valid_for_ms: Option<u64>,
        descriptor_set: &'static [u8],
    ) -> Self {
        Self {
            signature: MethodSignature::new(
                service,
                method,
                endpoint,
                MethodShape::Call,
                request,
                response,
                false,
                lease_valid_for_ms,
                descriptor_set,
            ),
            marker: PhantomData,
        }
    }

    /// Binds this method to one generated robot service instance.
    #[must_use]
    pub const fn bind(self, instance: &'static str, request: Request) -> Call<Request, Response> {
        Call {
            instance,
            method: self,
            request,
        }
    }

    /// Returns the complete generated contract method identity.
    #[must_use]
    pub const fn signature(self) -> MethodSignature {
        self.signature
    }

    /// Creates the explicit withdrawal for a leased method.
    #[must_use]
    pub const fn withdraw(self, instance: &'static str) -> Withdraw<Request, Response> {
        assert!(
            self.signature.lease.is_some(),
            "only leased calls can be withdrawn"
        );
        Withdraw {
            instance,
            method: self,
        }
    }
}

impl<Request, Response> Copy for CallMethod<Request, Response> {}
impl<Request, Response> Clone for CallMethod<Request, Response> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<Request, Response> fmt::Debug for CallMethod<Request, Response> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CallMethod")
            .field(&self.signature)
            .finish()
    }
}
impl<Request: 'static, Response: 'static> MethodDescriptor for CallMethod<Request, Response> {
    fn signature(self) -> MethodSignature {
        self.signature
    }
}

/// Contract-owned descriptor for an observation source.
pub struct ObservationMethod<Value> {
    signature: MethodSignature,
    marker: PhantomData<fn() -> Value>,
}

impl<Value> ObservationMethod<Value> {
    /// Creates one generated observation descriptor.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        service: &'static str,
        method: &'static str,
        endpoint: &'static str,
        request: &'static str,
        response: &'static str,
        retained_latest: bool,
        lease_valid_for_ms: Option<u64>,
        descriptor_set: &'static [u8],
    ) -> Self {
        Self {
            signature: MethodSignature::new(
                service,
                method,
                endpoint,
                MethodShape::Observation,
                request,
                response,
                retained_latest,
                lease_valid_for_ms,
                descriptor_set,
            ),
            marker: PhantomData,
        }
    }

    /// Binds this method to one generated robot service instance.
    #[must_use]
    pub const fn bind(self, instance: &'static str) -> Observation<Value> {
        Observation {
            instance,
            method: self,
        }
    }

    /// Returns the complete generated contract method identity.
    #[must_use]
    pub const fn signature(self) -> MethodSignature {
        self.signature
    }
}

impl<Value> Copy for ObservationMethod<Value> {}
impl<Value> Clone for ObservationMethod<Value> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<Value> fmt::Debug for ObservationMethod<Value> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ObservationMethod")
            .field(&self.signature)
            .finish()
    }
}
impl<Value: 'static> MethodDescriptor for ObservationMethod<Value> {
    fn signature(self) -> MethodSignature {
        self.signature
    }
}

/// One inert, instance-bound call.
#[derive(Debug)]
pub struct Call<Request, Response> {
    instance: &'static str,
    method: CallMethod<Request, Response>,
    request: Request,
}

impl<Request, Response> Call<Request, Response> {
    /// Returns the exact `robot.yaml` service instance identity.
    #[must_use]
    pub const fn instance(&self) -> &'static str {
        self.instance
    }

    /// Returns the typed contract method for session binding.
    #[must_use]
    pub const fn method(&self) -> CallMethod<Request, Response> {
        self.method
    }

    /// Returns the generated contract method identity.
    #[must_use]
    pub fn signature(&self) -> MethodSignature {
        self.method.signature
    }

    /// Borrows the typed request payload.
    #[must_use]
    pub const fn request(&self) -> &Request {
        &self.request
    }

    /// Consumes the operation into its inert parts.
    pub fn into_parts(self) -> (&'static str, MethodSignature, Request) {
        (self.instance, self.method.signature, self.request)
    }
}

/// One inert, instance-bound observation source.
#[derive(Debug)]
pub struct Observation<Value> {
    instance: &'static str,
    method: ObservationMethod<Value>,
}

impl<Value> Copy for Observation<Value> {}
impl<Value> Clone for Observation<Value> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<Value> Observation<Value> {
    /// Returns the exact `robot.yaml` service instance identity.
    #[must_use]
    pub const fn instance(self) -> &'static str {
        self.instance
    }

    /// Returns the typed contract method for session binding.
    #[must_use]
    pub const fn method(self) -> ObservationMethod<Value> {
        self.method
    }

    /// Returns the generated contract method identity.
    #[must_use]
    pub fn signature(self) -> MethodSignature {
        self.method.signature
    }
}

/// Explicit withdrawal of one leased instance-bound call.
#[derive(Debug)]
pub struct Withdraw<Request, Response> {
    instance: &'static str,
    method: CallMethod<Request, Response>,
}

impl<Request, Response> Copy for Withdraw<Request, Response> {}
impl<Request, Response> Clone for Withdraw<Request, Response> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<Request, Response> Withdraw<Request, Response> {
    /// Returns the exact `robot.yaml` service instance identity.
    #[must_use]
    pub const fn instance(self) -> &'static str {
        self.instance
    }

    /// Returns the leased contract method being withdrawn.
    #[must_use]
    pub fn signature(self) -> MethodSignature {
        self.method.signature
    }
}

/// Magic prefix used for framed descriptor payloads in native artifacts.
pub const DESCRIPTOR_FRAME_MAGIC: [u8; 8] = *b"PHXDESC1";

/// Number of bytes before a framed descriptor payload.
pub const DESCRIPTOR_FRAME_HEADER_BYTES: usize = 16;

/// Builds one bounded, length-delimited descriptor frame at compile time.
pub const fn descriptor_frame<const N: usize>(descriptor_set: &[u8]) -> [u8; N] {
    assert!(N == DESCRIPTOR_FRAME_HEADER_BYTES + descriptor_set.len());
    let mut frame = [0_u8; N];
    let mut index = 0;
    while index < DESCRIPTOR_FRAME_MAGIC.len() {
        frame[index] = DESCRIPTOR_FRAME_MAGIC[index];
        index += 1;
    }
    let length_bytes = (descriptor_set.len() as u64).to_le_bytes();
    index = 0;
    while index < length_bytes.len() {
        frame[8 + index] = length_bytes[index];
        index += 1;
    }
    index = 0;
    while index < descriptor_set.len() {
        frame[DESCRIPTOR_FRAME_HEADER_BYTES + index] = descriptor_set[index];
        index += 1;
    }
    frame
}

/// Shared robotics vocabulary: Rust-authored Protobuf messages and their
/// domain validation, usable without a concrete runtime implementation.
pub mod robotics;

/// Spatial quantities shared by component and service contracts.
pub mod geometry;

/// Standard component contracts: the payload vocabulary each device
/// capability kind publishes or accepts, referenced by capability-derived
/// component endpoints and simulated providers alike. Each vocabulary
/// feature opens its own kinds.
pub mod component;
