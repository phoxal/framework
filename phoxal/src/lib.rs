//! Phoxal's current runtime and public-session boundary.
//!
//! A [`runtime::Runtime`] owns one synchronous state machine and exchanges
//! generated Protobuf values through the execution protocol. Applications
//! attach through [`session`] and the supervisor owns the transport and
//! process lifecycle. Source preparation and immutable bundle assembly are
//! owned by `phoxal-project`; this crate only exposes the runtime and public
//! session boundaries that consume their completed products.

#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(all(feature = "runtime", test))]
extern crate self as phoxal;

pub mod geometry;
mod sample_schedule;

/// Opaque execution, producer, and timeline identities used by the runtime
/// and public protocols.
#[cfg(feature = "protocol")]
#[cfg_attr(docsrs, doc(cfg(feature = "protocol")))]
pub mod identity;

/// Framework-owned Protobuf protocol messages and transport-independent
/// admission state.
#[cfg(feature = "protocol")]
#[cfg_attr(docsrs, doc(cfg(feature = "protocol")))]
pub mod communication;

/// The private Zenoh transport owner used by runtime and supervisor code.
#[cfg(any(feature = "runtime", feature = "supervisor"))]
#[doc(hidden)]
pub(crate) mod bus;

#[cfg(feature = "supervisor")]
#[path = "bus/router.rs"]
pub(crate) mod router;

/// Synchronous Runtime authoring and execution.
#[cfg(feature = "runtime")]
#[cfg_attr(docsrs, doc(cfg(feature = "runtime")))]
pub mod runtime;

/// Public logical-session client.
#[cfg(feature = "session")]
#[cfg_attr(docsrs, doc(cfg(feature = "session")))]
pub mod session;

/// Public Protobuf transport implementation shared by sessions and the
/// supervisor host. It is not an SDK transport handle.
#[cfg(any(feature = "session", feature = "supervisor"))]
#[doc(hidden)]
#[path = "bus/public_session.rs"]
pub mod communication_transport;

/// Supervisor rendezvous paths and the supervisor-owned execution host.
#[cfg(feature = "supervisor")]
#[cfg_attr(docsrs, doc(cfg(feature = "supervisor")))]
pub mod supervisor {
    pub mod rendezvous;

    #[doc(hidden)]
    pub mod host;
}

/// Framework result type backed by `anyhow`.
pub use anyhow::Result;

/// Inert generated public-port descriptors.
#[cfg(feature = "port")]
#[cfg_attr(docsrs, doc(cfg(feature = "port")))]
pub use phoxal_port as port;

#[cfg(feature = "runtime")]
#[cfg_attr(docsrs, doc(cfg(feature = "runtime")))]
pub use phoxal_macros::{Config, runtime};

pub use sample_schedule::{MissedTickPolicy, SampleSchedule};

/// The current runtime attribute namespace.
#[cfg(feature = "runtime")]
#[doc(hidden)]
pub mod __private {
    pub use crate::port::{PortDescriptor, PortKind};

    pub trait StatePortValue<Value>: PortDescriptor {}
    pub trait SamplePortValue<Value>: PortDescriptor {}
    pub trait EventPortValue<Value>: PortDescriptor {}
    pub trait StreamPortValue<Value>: PortDescriptor {}
    pub trait SetpointPortValue<Value>: PortDescriptor {}
    pub trait ReadPortValue<Request, Response>: PortDescriptor {}
    pub trait CommandsPortValue<Request, Response>: PortDescriptor {}

    impl<T: 'static> StatePortValue<T> for crate::port::State<T> {}
    impl<T: 'static> StatePortValue<crate::runtime::Sample<T>> for crate::port::State<T> {}
    impl<T: 'static> SamplePortValue<T> for crate::port::Sample<T> {}
    impl<T: 'static> EventPortValue<T> for crate::port::Event<T> {}
    impl<T: 'static> StreamPortValue<T> for crate::port::Stream<T> {}
    impl<T: 'static> SetpointPortValue<T> for crate::port::Setpoint<T> {}
    impl<T: 'static> SetpointPortValue<Option<&T>> for crate::port::Setpoint<T> {}
    impl<T: 'static> SetpointPortValue<Option<T>> for crate::port::Setpoint<T> {}
    impl<Request: 'static, Response: 'static> ReadPortValue<Request, Response>
        for crate::port::Read<Request, Response>
    {
    }
    impl<Request: 'static, Response: 'static> CommandsPortValue<Request, Response>
        for crate::port::Commands<Request, Response>
    {
    }

    pub fn assert_state_port<P, Value>(port: P)
    where
        P: StatePortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::State);
    }

    pub fn assert_sample_port<P, Value>(port: P)
    where
        P: SamplePortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Sample);
    }

    pub fn assert_event_port<P, Value>(port: P)
    where
        P: EventPortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Event);
    }

    pub fn assert_stream_port<P, Value>(port: P)
    where
        P: StreamPortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Stream);
    }

    pub fn assert_setpoint_port<P, Value>(port: P)
    where
        P: SetpointPortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Setpoint);
    }

    pub fn assert_read_port<P, Request, Response>(port: P)
    where
        P: ReadPortValue<Request, Response>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Read);
    }

    pub fn assert_commands_port<P, Request, Response>(port: P)
    where
        P: CommandsPortValue<Request, Response>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Commands);
    }

    fn assert_kind<P: PortDescriptor>(expected: PortKind) {
        assert!(
            P::KIND == expected,
            "port descriptor kind does not match runtime role"
        );
    }
}
