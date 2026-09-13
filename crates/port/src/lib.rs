//! Inert typed references to public ports declared by Phoxal service contracts.
//!
//! Contract build tooling generates constants of these types from Protobuf
//! service methods.
//! A descriptor carries only a public port name and its Rust payload types.
//! Constructing one performs no registration, discovery, transport I/O, or
//! process selection.

use std::any::Any;
use std::fmt;
use std::marker::PhantomData;

/// An erased encoder for one generated Protobuf message.
pub type EncodeFn = fn(&dyn Any) -> Result<Vec<u8>, CodecError>;

/// An erased decoder for one generated Protobuf message.
pub type DecodeFn = fn(&[u8]) -> Result<Box<dyn Any + Send + Sync>, CodecError>;

/// Failure from a generated typed port codec.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodecError {
    /// The descriptor was generated without a wire codec.
    Unavailable,
    /// The caller supplied a Rust value of a different type than the descriptor.
    TypeMismatch,
    /// Protobuf encoding failed.
    Encode,
    /// Protobuf decoding failed.
    Decode,
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "port has no generated wire codec",
            Self::TypeMismatch => "port codec received a value of the wrong Rust type",
            Self::Encode => "Protobuf encoding failed",
            Self::Decode => "Protobuf decoding failed",
        })
    }
}

impl std::error::Error for CodecError {}

/// Erased request/response codecs retained by generated port descriptors.
///
/// The descriptor remains inert: these function pointers perform no I/O and
/// do not register anything.  They only let a runtime or client use the exact
/// Prost type selected by the owner's generated service interface.
#[derive(Clone, Copy, Debug)]
pub struct PortCodec {
    request_encode: Option<EncodeFn>,
    request_decode: Option<DecodeFn>,
    response_encode: Option<EncodeFn>,
    response_decode: Option<DecodeFn>,
}

impl PortCodec {
    /// Creates a codec pair for a generated request/response port.
    #[must_use]
    pub const fn new(
        request_encode: Option<EncodeFn>,
        request_decode: Option<DecodeFn>,
        response_encode: Option<EncodeFn>,
        response_decode: Option<DecodeFn>,
    ) -> Self {
        Self {
            request_encode,
            request_decode,
            response_encode,
            response_decode,
        }
    }

    /// Encodes a request, rejecting descriptors without a request body.
    pub fn encode_request(&self, value: &dyn Any) -> Result<Vec<u8>, CodecError> {
        self.request_encode
            .ok_or(CodecError::Unavailable)
            .and_then(|encode| encode(value))
    }

    /// Decodes a request into the generated Rust message type.
    pub fn decode_request(&self, bytes: &[u8]) -> Result<Box<dyn Any + Send + Sync>, CodecError> {
        self.request_decode
            .ok_or(CodecError::Unavailable)
            .and_then(|decode| decode(bytes))
    }

    /// Encodes a response or publication payload.
    pub fn encode_response(&self, value: &dyn Any) -> Result<Vec<u8>, CodecError> {
        self.response_encode
            .ok_or(CodecError::Unavailable)
            .and_then(|encode| encode(value))
    }

    /// Decodes a response or publication payload into its generated type.
    pub fn decode_response(&self, bytes: &[u8]) -> Result<Box<dyn Any + Send + Sync>, CodecError> {
        self.response_decode
            .ok_or(CodecError::Unavailable)
            .and_then(|decode| decode(bytes))
    }

    /// Returns a codec containing only this descriptor's request functions.
    #[must_use]
    pub const fn request_only(self) -> Self {
        Self::new(self.request_encode, self.request_decode, None, None)
    }

    /// Returns a codec containing only this descriptor's response functions.
    #[must_use]
    pub const fn response_only(self) -> Self {
        Self::new(None, None, self.response_encode, self.response_decode)
    }
}

/// The semantic kind of a public service port.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum PortKind {
    /// A latest published state projection.
    State,
    /// An ordered batch of captured observations.
    Sample,
    /// An ordered batch of discrete occurrences.
    Event,
    /// An ordered stream with explicit gap and termination semantics.
    Stream,
    /// A replaceable intent with validity and renewal rules.
    Setpoint,
    /// An immutable request and response over an accepted projection.
    Read,
    /// A behavioral request and its processing decision or result.
    Commands,
}

impl PortKind {
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
            Self::Commands => "commands",
        }
    }
}

/// The complete identity of one generated public port.
///
/// The request and response names are fully-qualified Protobuf message names.
/// Publication ports use `google.protobuf.Empty` as their request identity.
/// The optional descriptor frame is retained by generated owner bindings so a
/// release artifact can carry the unchanged descriptor closure without
/// executing the binary.
#[derive(Clone, Copy, Debug)]
pub struct PortSignature {
    /// Public port name.
    pub name: &'static str,
    /// Fully-qualified owning Protobuf service name.
    pub service: &'static str,
    /// Protobuf method name within the owning service.
    pub method: &'static str,
    /// Semantic port kind.
    pub kind: PortKind,
    /// Fully-qualified request message name.
    pub request: &'static str,
    /// Fully-qualified response message name.
    pub response: &'static str,
    descriptor_set: &'static [u8],
    codec: Option<PortCodec>,
}

impl PartialEq for PortSignature {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.service == other.service
            && self.method == other.method
            && self.kind == other.kind
            && self.request == other.request
            && self.response == other.response
            && self.descriptor_set == other.descriptor_set
    }
}

impl Eq for PortSignature {}

impl std::hash::Hash for PortSignature {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.service.hash(state);
        self.method.hash(state);
        self.kind.hash(state);
        self.request.hash(state);
        self.response.hash(state);
        self.descriptor_set.hash(state);
    }
}

impl PortSignature {
    /// Creates an identity without an embedded descriptor set.
    #[must_use]
    pub const fn new(
        name: &'static str,
        service: &'static str,
        method: &'static str,
        kind: PortKind,
        request: &'static str,
        response: &'static str,
    ) -> Self {
        Self::with_descriptor(name, service, method, kind, request, response, &[])
    }

    /// Creates an identity retaining the owner's original descriptor bytes.
    #[must_use]
    pub const fn with_descriptor(
        name: &'static str,
        service: &'static str,
        method: &'static str,
        kind: PortKind,
        request: &'static str,
        response: &'static str,
        descriptor_set: &'static [u8],
    ) -> Self {
        Self {
            name,
            service,
            method,
            kind,
            request,
            response,
            descriptor_set,
            codec: None,
        }
    }

    /// Creates an identity retaining its descriptor closure and generated
    /// request/response Prost codecs.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn with_descriptor_and_codec(
        name: &'static str,
        service: &'static str,
        method: &'static str,
        kind: PortKind,
        request: &'static str,
        response: &'static str,
        descriptor_set: &'static [u8],
        codec: PortCodec,
    ) -> Self {
        Self {
            name,
            service,
            method,
            kind,
            request,
            response,
            descriptor_set,
            codec: Some(codec),
        }
    }

    /// Returns the framed original descriptor closure retained by the owner.
    #[must_use]
    pub const fn descriptor_set(self) -> &'static [u8] {
        self.descriptor_set
    }

    /// Returns the generated wire codec, when this descriptor came from a
    /// Protobuf service build.
    #[must_use]
    pub const fn codec(self) -> Option<PortCodec> {
        self.codec
    }
}

/// Magic prefix used for framed descriptor payloads in native artifact
/// sections.
pub const DESCRIPTOR_FRAME_MAGIC: [u8; 8] = *b"PHXDESC0";

/// Number of bytes before a framed descriptor payload.
pub const DESCRIPTOR_FRAME_HEADER_BYTES: usize = 16;

/// Builds one bounded, length-delimited descriptor frame at compile time.
///
/// A length prefix is required because linkers concatenate same-named section
/// fragments from every object file and may add alignment padding between
/// them.  The generated owner code supplies the exact output array length.
pub const fn descriptor_frame<const N: usize>(descriptor_set: &[u8]) -> [u8; N] {
    assert!(N == DESCRIPTOR_FRAME_HEADER_BYTES + descriptor_set.len());
    let mut frame = [0_u8; N];
    let mut index = 0;
    while index < DESCRIPTOR_FRAME_MAGIC.len() {
        frame[index] = DESCRIPTOR_FRAME_MAGIC[index];
        index += 1;
    }
    let length = descriptor_set.len() as u64;
    let length_bytes = length.to_le_bytes();
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

/// Common metadata exposed by every typed port reference.
pub trait PortDescriptor: Copy + fmt::Debug + Send + Sync + 'static {
    /// The semantic kind fixed by the owning Protobuf method.
    const KIND: PortKind;

    /// The public port name fixed by the owning Protobuf method.
    fn name(self) -> &'static str;

    /// The complete method identity fixed by the owning Protobuf method.
    fn signature(self) -> PortSignature;
}

macro_rules! payload_descriptor {
    ($name:ident, $kind:ident, $summary:literal) => {
        #[doc = $summary]
        pub struct $name<T> {
            signature: PortSignature,
            payload: PhantomData<fn() -> T>,
        }

        impl<T> $name<T> {
            /// Creates an inert descriptor for a generated public port name.
            #[must_use]
            pub const fn new(name: &'static str) -> Self {
                Self {
                    signature: PortSignature::new(name, "", "", PortKind::$kind, "", ""),
                    payload: PhantomData,
                }
            }

            /// Creates a typed descriptor with its generated Protobuf identity.
            #[must_use]
            pub const fn with_signature(
                name: &'static str,
                service: &'static str,
                method: &'static str,
                request: &'static str,
                response: &'static str,
                descriptor_set: &'static [u8],
            ) -> Self {
                Self {
                    signature: PortSignature::with_descriptor(
                        name,
                        service,
                        method,
                        PortKind::$kind,
                        request,
                        response,
                        descriptor_set,
                    ),
                    payload: PhantomData,
                }
            }

            /// Creates a typed descriptor retaining its generated Protobuf
            /// codec and descriptor closure.
            #[must_use]
            pub const fn with_codec_signature(
                name: &'static str,
                service: &'static str,
                method: &'static str,
                request: &'static str,
                response: &'static str,
                descriptor_set: &'static [u8],
                codec: PortCodec,
            ) -> Self {
                Self {
                    signature: PortSignature::with_descriptor_and_codec(
                        name,
                        service,
                        method,
                        PortKind::$kind,
                        request,
                        response,
                        descriptor_set,
                        codec,
                    ),
                    payload: PhantomData,
                }
            }

            /// Returns the public port name.
            #[must_use]
            pub const fn name(self) -> &'static str {
                self.signature.name
            }

            /// Returns the complete generated Protobuf identity.
            #[must_use]
            pub const fn signature(self) -> PortSignature {
                self.signature
            }
        }

        impl<T> Clone for $name<T> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<T> Copy for $name<T> {}

        impl<T> fmt::Debug for $name<T> {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .field("name", &self.signature.name)
                    .finish()
            }
        }

        impl<T: 'static> PortDescriptor for $name<T> {
            const KIND: PortKind = PortKind::$kind;

            fn name(self) -> &'static str {
                self.signature.name
            }

            fn signature(self) -> PortSignature {
                self.signature
            }
        }
    };
}

payload_descriptor!(State, State, "A typed latest-state publication port.");
payload_descriptor!(Sample, Sample, "A typed captured-sample publication port.");
payload_descriptor!(Event, Event, "A typed discrete-event publication port.");
payload_descriptor!(Stream, Stream, "A typed ordered-stream publication port.");
payload_descriptor!(
    Setpoint,
    Setpoint,
    "A typed replaceable-setpoint publication port."
);

macro_rules! exchange_descriptor {
    ($name:ident, $kind:ident, $summary:literal) => {
        #[doc = $summary]
        pub struct $name<Request, Response> {
            signature: PortSignature,
            exchange: PhantomData<fn(Request) -> Response>,
        }

        impl<Request, Response> $name<Request, Response> {
            /// Creates an inert descriptor for a generated public port name.
            #[must_use]
            pub const fn new(name: &'static str) -> Self {
                Self {
                    signature: PortSignature::new(name, "", "", PortKind::$kind, "", ""),
                    exchange: PhantomData,
                }
            }

            /// Creates a typed descriptor with its generated Protobuf identity.
            #[must_use]
            pub const fn with_signature(
                name: &'static str,
                service: &'static str,
                method: &'static str,
                request: &'static str,
                response: &'static str,
                descriptor_set: &'static [u8],
            ) -> Self {
                Self {
                    signature: PortSignature::with_descriptor(
                        name,
                        service,
                        method,
                        PortKind::$kind,
                        request,
                        response,
                        descriptor_set,
                    ),
                    exchange: PhantomData,
                }
            }

            /// Creates a typed exchange descriptor retaining its generated
            /// request/response Protobuf codec and descriptor closure.
            #[must_use]
            pub const fn with_codec_signature(
                name: &'static str,
                service: &'static str,
                method: &'static str,
                request: &'static str,
                response: &'static str,
                descriptor_set: &'static [u8],
                codec: PortCodec,
            ) -> Self {
                Self {
                    signature: PortSignature::with_descriptor_and_codec(
                        name,
                        service,
                        method,
                        PortKind::$kind,
                        request,
                        response,
                        descriptor_set,
                        codec,
                    ),
                    exchange: PhantomData,
                }
            }

            /// Returns the public port name.
            #[must_use]
            pub const fn name(self) -> &'static str {
                self.signature.name
            }

            /// Returns the complete generated Protobuf identity.
            #[must_use]
            pub const fn signature(self) -> PortSignature {
                self.signature
            }
        }

        impl<Request, Response> Clone for $name<Request, Response> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<Request, Response> Copy for $name<Request, Response> {}

        impl<Request, Response> fmt::Debug for $name<Request, Response> {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .field("name", &self.signature.name)
                    .finish()
            }
        }

        impl<Request: 'static, Response: 'static> PortDescriptor for $name<Request, Response> {
            const KIND: PortKind = PortKind::$kind;

            fn name(self) -> &'static str {
                self.signature.name
            }

            fn signature(self) -> PortSignature {
                self.signature
            }
        }
    };
}

exchange_descriptor!(Read, Read, "A typed immutable-read port.");
exchange_descriptor!(Commands, Commands, "A typed behavioral-command port.");

#[cfg(test)]
mod tests {
    use super::{Commands, Event, PortDescriptor, PortKind, Read, State};

    struct Payload;
    struct Request;
    struct Response;

    const STATUS: State<Payload> = State::new("status");
    const FINISHED: Event<Payload> = Event::new("finished");
    const CURRENT: Read<Request, Response> = Read::new("current");
    const COMMANDS: Commands<Request, Response> = Commands::new("commands");

    #[test]
    fn descriptors_carry_only_the_owned_name_and_kind() {
        assert_eq!(STATUS.name(), "status");
        assert_eq!(FINISHED.name(), "finished");
        assert_eq!(CURRENT.name(), "current");
        assert_eq!(COMMANDS.name(), "commands");
        assert_eq!(<State<Payload> as PortDescriptor>::KIND, PortKind::State);
        assert_eq!(
            <Commands<Request, Response> as PortDescriptor>::KIND,
            PortKind::Commands
        );
    }

    #[test]
    fn descriptors_do_not_require_payload_traits() {
        let copied = STATUS;
        let cloned = copied;
        assert_eq!(cloned.name(), "status");
        assert_eq!(format!("{cloned:?}"), "State { name: \"status\" }");
    }
}
