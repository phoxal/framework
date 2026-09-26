//! Phoxal's current runtime and public-session boundary.
//!
//! A `runtime::Runtime` (available with the `runtime` feature) owns one synchronous state machine and exchanges
//! generated Protobuf values through the execution protocol. Applications
//! attach through the `session` module and the supervisor owns the transport and
//! process lifecycle. Source preparation and immutable bundle assembly are
//! owned by `cargo-phoxal`; this crate only exposes the runtime and public
//! session boundaries that consume their completed products.

#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(all(feature = "runtime", test))]
extern crate self as phoxal;

pub mod geometry;
#[cfg(feature = "runtime")]
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

/// Serialized project, bundle, scenario, and simulation contracts.
#[cfg(feature = "artifact")]
#[cfg_attr(docsrs, doc(cfg(feature = "artifact")))]
pub mod artifact;

/// Synchronous Runtime authoring and execution.
#[cfg(feature = "runtime")]
#[cfg_attr(docsrs, doc(cfg(feature = "runtime")))]
pub mod runtime;

/// Scenario authoring and execution surface.
/// Independent of `runtime` so consumer crates that
/// only define scenarios do not pull Zenoh/tokio/clap.
#[cfg(feature = "scenario")]
#[cfg_attr(docsrs, doc(cfg(feature = "scenario")))]
pub mod scenario;

/// Public logical-session client.
#[cfg(feature = "session")]
#[cfg_attr(docsrs, doc(cfg(feature = "session")))]
pub mod session;

/// Public Protobuf transport implementation shared by sessions and the
/// supervisor host. It is not an SDK transport handle; sessions and the
/// supervisor are its consumers.
#[cfg(feature = "session")]
pub mod communication_transport;

/// Framework result type backed by `anyhow`.
#[cfg(any(feature = "runtime", feature = "scenario"))]
pub use anyhow::{Result, anyhow};

/// Inert generated service-method descriptors and robot-instance operations.
#[cfg(feature = "contract")]
#[cfg_attr(docsrs, doc(cfg(feature = "contract")))]
pub mod contract;

/// Attaches the build-script generated API once at the crate root.
#[macro_export]
macro_rules! api {
    () => {
        pub mod api {
            include!(concat!(env!("OUT_DIR"), "/phoxal_api.rs"));
        }
    };
}

/// Implementation dependencies used by generated API code.
///
/// Generated bindings reference this module for their Protobuf runtime and
/// the build-helper version marker; it is not application API.
#[cfg(feature = "contract")]
#[cfg_attr(docsrs, doc(cfg(feature = "contract")))]
pub mod generated {
    pub use prost;

    const fn version_marker(version: &str) -> u64 {
        let bytes = version.as_bytes();
        let mut hash = 0xcbf29ce484222325_u64;
        let mut index = 0;
        while index < bytes.len() {
            hash ^= bytes[index] as u64;
            hash = hash.wrapping_mul(0x100000001b3);
            index += 1;
        }
        hash
    }

    pub const API_GENERATOR_MARKER: u64 = version_marker(env!("CARGO_PKG_VERSION"));
}

// Private compatibility descriptors used only by runtime and scenario
// implementation adapters during the direct generated-method cutover.
#[cfg(any(feature = "runtime", feature = "scenario"))]
mod port;

/// Framework-owned shared robotics vocabulary: generated Protobuf messages
/// and their domain validation. Independent of the protocol/transport
/// features so a component or service that only needs the inert port surface
/// does not pay for the robotics codegen.
#[cfg(feature = "robotics")]
#[cfg_attr(docsrs, doc(cfg(feature = "robotics")))]
pub mod robotics;

/// Authoring helper for Protobuf build scripts.
///
/// Re-exports the Protobuf contract compiler, dependency descriptor input,
/// and the `include_dir` / `descriptor_set_path` lookups over the internal
/// `phoxal-build` implementation helper.
///
/// Owner manifests declare `phoxal` with the `build` feature in
/// `[build-dependencies]`; the standard contract authoring path does not
/// name `phoxal-build` or `phoxal-port` directly. This module pulls in no
/// runtime/transport/session/supervisor/native simulation dependencies.
#[cfg(feature = "build")]
#[cfg_attr(docsrs, doc(cfg(feature = "build")))]
pub mod build;

#[cfg(feature = "runtime")]
#[cfg_attr(docsrs, doc(cfg(feature = "runtime")))]
pub use phoxal_macros::{Config, runtime};

/// `#[phoxal::scenario]` attribute for function-based simulation tests.
#[cfg(feature = "scenario")]
#[cfg_attr(docsrs, doc(cfg(feature = "scenario")))]
pub use phoxal_macros::scenario;

#[cfg(feature = "runtime")]
pub use sample_schedule::{MissedTickPolicy, SampleSchedule};

/// Macro- and generator-support surface: port descriptors, shared error
/// plumbing, and the compile-time checks emitted by `#[phoxal::runtime]`
/// expansions.  Not application API.
#[cfg(any(feature = "runtime", feature = "scenario"))]
pub mod macro_support {
    pub use crate::port::*;
    #[cfg(feature = "runtime")]
    pub use anyhow;

    #[cfg(feature = "runtime")]
    pub trait StatePortValue<Value>: PortDescriptor {}
    #[cfg(feature = "runtime")]
    pub trait SamplePortValue<Value>: PortDescriptor {}
    #[cfg(feature = "runtime")]
    pub trait EventPortValue<Value>: PortDescriptor {}
    #[cfg(feature = "runtime")]
    pub trait StreamPortValue<Value>: PortDescriptor {}
    #[cfg(feature = "runtime")]
    pub trait SetpointPortValue<Value>: PortDescriptor {}
    #[cfg(feature = "runtime")]
    pub trait ReadPortValue<Request, Response>: PortDescriptor {}
    #[cfg(feature = "runtime")]
    pub trait CommandsPortValue<Request, Response>: PortDescriptor {}

    #[cfg(feature = "runtime")]
    impl<T: 'static> StatePortValue<T> for crate::port::State<T> {}
    #[cfg(feature = "runtime")]
    impl<T: 'static> StatePortValue<crate::runtime::Sample<T>> for crate::port::State<T> {}
    #[cfg(feature = "runtime")]
    impl<T: 'static> SamplePortValue<T> for crate::port::Sample<T> {}
    #[cfg(feature = "runtime")]
    impl<T: 'static> EventPortValue<T> for crate::port::Event<T> {}
    #[cfg(feature = "runtime")]
    impl<T: 'static> StreamPortValue<T> for crate::port::Stream<T> {}
    #[cfg(feature = "runtime")]
    impl<T: 'static> SetpointPortValue<T> for crate::port::Setpoint<T> {}
    #[cfg(feature = "runtime")]
    impl<T: 'static> SetpointPortValue<Option<&T>> for crate::port::Setpoint<T> {}
    #[cfg(feature = "runtime")]
    impl<T: 'static> SetpointPortValue<Option<T>> for crate::port::Setpoint<T> {}
    #[cfg(feature = "runtime")]
    impl<Request: 'static, Response: 'static> ReadPortValue<Request, Response>
        for crate::port::Read<Request, Response>
    {
    }
    #[cfg(feature = "runtime")]
    impl<Request: 'static, Response: 'static> CommandsPortValue<Request, Response>
        for crate::port::Commands<Request, Response>
    {
    }

    #[cfg(feature = "runtime")]
    pub fn assert_state_port<P, Value>(port: P)
    where
        P: StatePortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::State);
    }

    #[cfg(feature = "runtime")]
    pub fn assert_sample_port<P, Value>(port: P)
    where
        P: SamplePortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Sample);
    }

    #[cfg(feature = "runtime")]
    pub fn assert_event_port<P, Value>(port: P)
    where
        P: EventPortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Event);
    }

    #[cfg(feature = "runtime")]
    pub fn assert_stream_port<P, Value>(port: P)
    where
        P: StreamPortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Stream);
    }

    #[cfg(feature = "runtime")]
    pub fn assert_setpoint_port<P, Value>(port: P)
    where
        P: SetpointPortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Setpoint);
    }

    #[cfg(feature = "runtime")]
    pub fn assert_read_port<P, Request, Response>(port: P)
    where
        P: ReadPortValue<Request, Response>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Read);
    }

    #[cfg(feature = "runtime")]
    pub fn assert_commands_port<P, Request, Response>(port: P)
    where
        P: CommandsPortValue<Request, Response>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Commands);
    }

    #[cfg(feature = "runtime")]
    fn assert_kind<P: PortDescriptor>(expected: PortKind) {
        assert!(
            P::KIND == expected,
            "port descriptor kind does not match runtime role"
        );
    }
}

/// Linked framework package version for executable information responses.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
