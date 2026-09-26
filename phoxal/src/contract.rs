//! Inert generated service methods and robot-instance operations.
//!
//! Protobuf cardinality determines whether a method is a one-shot call or an
//! observation source. The generated robot facade binds these contract-owned
//! method descriptors to one exact `robot.yaml` service instance. Constructing
//! a value in this module performs no I/O and grants no authority.

use std::fmt;
use std::marker::PhantomData;

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

    /// Adapts a non-leased call to the existing provider ingress collector.
    #[cfg(feature = "runtime")]
    #[must_use]
    pub const fn commands_port(self) -> crate::port::Commands<Request, Response> {
        assert!(
            self.signature.lease.is_none(),
            "leased calls use the setpoint provider adapter"
        );
        crate::port::Commands::from_signature(crate::port::PortSignature::from_method(
            self.signature,
            crate::port::PortKind::Commands,
        ))
    }

    /// Adapts a leased call to the existing provider ingress collector.
    #[cfg(feature = "runtime")]
    #[must_use]
    pub const fn setpoint_port(self) -> crate::port::Setpoint<Request> {
        assert!(
            self.signature.lease.is_some(),
            "only leased calls use the setpoint provider adapter"
        );
        crate::port::Setpoint::from_signature(crate::port::PortSignature::from_method(
            self.signature,
            crate::port::PortKind::Setpoint,
        ))
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

    /// Adapts a retained observation to the existing provider projection collector.
    #[cfg(feature = "runtime")]
    #[must_use]
    pub const fn state_port(self) -> crate::port::State<Value> {
        assert!(
            self.signature.retained_latest,
            "only retained observations use the state provider adapter"
        );
        crate::port::State::from_signature(crate::port::PortSignature::from_method(
            self.signature,
            crate::port::PortKind::State,
        ))
    }

    /// Adapts a non-retained observation to the existing provider sample collector.
    #[cfg(feature = "runtime")]
    #[must_use]
    pub const fn sample_port(self) -> crate::port::Sample<Value> {
        assert!(
            !self.signature.retained_latest,
            "retained observations use the state provider adapter"
        );
        crate::port::Sample::from_signature(crate::port::PortSignature::from_method(
            self.signature,
            crate::port::PortKind::Sample,
        ))
    }

    /// Adapts a non-retained observation to an event provider collector.
    #[cfg(feature = "runtime")]
    #[must_use]
    pub const fn event_port(self) -> crate::port::Event<Value> {
        assert!(
            !self.signature.retained_latest,
            "retained observations use the state provider adapter"
        );
        crate::port::Event::from_signature(crate::port::PortSignature::from_method(
            self.signature,
            crate::port::PortKind::Event,
        ))
    }

    /// Adapts a non-retained observation to a stream provider collector.
    #[cfg(feature = "runtime")]
    #[must_use]
    pub const fn stream_port(self) -> crate::port::Stream<Value> {
        assert!(
            !self.signature.retained_latest,
            "retained observations use the state provider adapter"
        );
        crate::port::Stream::from_signature(crate::port::PortSignature::from_method(
            self.signature,
            crate::port::PortKind::Stream,
        ))
    }

    /// Adapts a leased observation to the existing provider setpoint collector.
    #[cfg(feature = "runtime")]
    #[must_use]
    pub const fn setpoint_port(self) -> crate::port::Setpoint<Value> {
        assert!(
            self.signature.lease.is_some(),
            "only leased observations use the setpoint provider adapter"
        );
        crate::port::Setpoint::from_signature(crate::port::PortSignature::from_method(
            self.signature,
            crate::port::PortKind::Setpoint,
        ))
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
