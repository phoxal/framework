//! Phoxal's current runtime and public-session boundary.
//!
//! A `runtime::Runtime` (available with the `runtime` feature) owns one synchronous state machine and exchanges
//! generated Protobuf values through the execution protocol. Applications
//! attach through the `session` module and the supervisor owns the transport and
//! process lifecycle. Source preparation and immutable bundle assembly are
//! owned by `cargo-phoxal`; this crate only exposes the runtime and public
//! session boundaries that consume their completed products.

#![cfg_attr(docsrs, feature(doc_cfg))]

// Macro-generated code references the SDK through `::phoxal`, including
// inside this crate's own modules; the self-alias keeps those paths valid
// for every consumer profile. Profiles without macro-generated paths leave
// the alias unused, which is expected.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

#[allow(unused_extern_crates)]
extern crate self as phoxal;

pub mod geometry;
#[cfg(feature = "runtime")]
mod sample_schedule;

/// Opaque execution, producer, and timeline identities used by the runtime
/// and public protocols.
pub mod identity;

/// Framework-owned Protobuf protocol messages and transport-independent
/// admission state.
pub mod communication;

/// Serialized project, bundle, scenario, and simulation contracts.
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

/// The canonical public contract surface: endpoint primitives, shared
/// robotics vocabulary, and standard component contracts.
pub mod contracts;

/// Compiler-resolved schema records for Rust-authored Protobuf messages.
pub mod schema;

/// Attaches the build-script generated API once at the crate root.
#[macro_export]
macro_rules! api {
    () => {
        pub mod api {
            include!(concat!(env!("OUT_DIR"), "/phoxal_api.rs"));
        }
    };
}

/// Protobuf runtime used by generated API code.
pub mod generated {
    pub use prost;
}

/// Generates bindings from local prepared contracts and component capabilities.
///
/// Used in a package build script after `cargo phoxal prepare` selects and
/// acquires the participants. It never downloads sources or invokes Cargo.
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
pub use phoxal_macros::{Config, endpoints, runtime};

/// Rust-authored Protobuf message and contract-module declarations.
pub use phoxal_macros::{message, messages};

#[cfg(feature = "runtime")]
pub use sample_schedule::{MissedTickPolicy, SampleSchedule};

/// Type checks used by generated endpoint collectors.
#[cfg(any(feature = "runtime", feature = "scenario"))]
pub mod macro_support {
    pub use crate::contracts::{MethodDescriptor, MethodSignature};
    #[cfg(feature = "runtime")]
    pub use anyhow;

    /// A publication's Rust projection matches its canonical observation payload.
    pub trait ObservationValue<Value>: MethodDescriptor {}
    impl<T: 'static> ObservationValue<T> for crate::contracts::ObservationMethod<T> {}
    #[cfg(feature = "runtime")]
    impl<T: 'static> ObservationValue<crate::runtime::Sample<T>>
        for crate::contracts::ObservationMethod<T>
    {
    }

    /// A leased projection or ingress uses the declared canonical payload.
    pub trait LeasedValue<Value>: MethodDescriptor {}
    impl<T: 'static> LeasedValue<T> for crate::contracts::ObservationMethod<T> {}
    impl<T: 'static> LeasedValue<Option<T>> for crate::contracts::ObservationMethod<T> {}
    impl<T: 'static> LeasedValue<Option<&T>> for crate::contracts::ObservationMethod<T> {}
    impl<T: 'static> LeasedValue<T> for crate::contracts::CallMethod<T, crate::contracts::Empty> {}

    /// A call handler exchanges the canonical request and response types.
    pub trait CallValue<Request, Response>: MethodDescriptor {}
    impl<Request: 'static, Response: 'static> CallValue<Request, Response>
        for crate::contracts::CallMethod<Request, Response>
    {
    }

    pub fn assert_retained_observation<P: ObservationValue<Value>, Value>(method: P) {
        assert!(method.signature().retained_latest);
    }
    pub fn assert_queued_observation<P: ObservationValue<Value>, Value>(method: P) {
        assert!(!method.signature().retained_latest);
    }
    pub fn assert_leased_method<P: LeasedValue<Value>, Value>(method: P) {
        assert!(method.signature().lease.is_some());
    }
    pub fn assert_call_method<P: CallValue<Request, Response>, Request, Response>(method: P) {
        assert!(method.signature().lease.is_none());
    }
}

/// Linked framework package version for executable information responses.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
