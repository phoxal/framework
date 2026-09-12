//! Inert typed references to public ports declared by Phoxal service contracts.
//!
//! Contract build tooling generates constants of these types from Protobuf
//! service methods.
//! A descriptor carries only a public port name and its Rust payload types.
//! Constructing one performs no registration, discovery, transport I/O, or
//! process selection.

use std::fmt;
use std::marker::PhantomData;

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

/// Common metadata exposed by every typed port reference.
pub trait PortDescriptor: Copy + fmt::Debug + Send + Sync + 'static {
    /// The semantic kind fixed by the owning Protobuf method.
    const KIND: PortKind;

    /// The public port name fixed by the owning Protobuf method.
    fn name(self) -> &'static str;
}

macro_rules! payload_descriptor {
    ($name:ident, $kind:ident, $summary:literal) => {
        #[doc = $summary]
        pub struct $name<T> {
            name: &'static str,
            payload: PhantomData<fn() -> T>,
        }

        impl<T> $name<T> {
            /// Creates an inert descriptor for a generated public port name.
            #[must_use]
            pub const fn new(name: &'static str) -> Self {
                Self {
                    name,
                    payload: PhantomData,
                }
            }

            /// Returns the public port name.
            #[must_use]
            pub const fn name(self) -> &'static str {
                self.name
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
                    .field("name", &self.name)
                    .finish()
            }
        }

        impl<T: 'static> PortDescriptor for $name<T> {
            const KIND: PortKind = PortKind::$kind;

            fn name(self) -> &'static str {
                self.name
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
            name: &'static str,
            exchange: PhantomData<fn(Request) -> Response>,
        }

        impl<Request, Response> $name<Request, Response> {
            /// Creates an inert descriptor for a generated public port name.
            #[must_use]
            pub const fn new(name: &'static str) -> Self {
                Self {
                    name,
                    exchange: PhantomData,
                }
            }

            /// Returns the public port name.
            #[must_use]
            pub const fn name(self) -> &'static str {
                self.name
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
                    .field("name", &self.name)
                    .finish()
            }
        }

        impl<Request: 'static, Response: 'static> PortDescriptor for $name<Request, Response> {
            const KIND: PortKind = PortKind::$kind;

            fn name(self) -> &'static str {
                self.name
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
