//! Framework-owned public protocol messages and admission invariants.
//!
//! These contracts are independent of runner implementation and package
//! versions.
//! They use standard Protobuf messages and remain available to session clients
//! without importing project tooling, official services, or simulation engines.

/// Fixed offer-only bootstrap protocol.
pub mod bootstrap {
    include!(concat!(env!("OUT_DIR"), "/phoxal.bootstrap.v1.rs"));
}

/// Public logical-session protocol.
pub mod session {
    include!(concat!(env!("OUT_DIR"), "/phoxal.session.v1.rs"));
}

/// Supervisor-to-runtime execution coordination protocol.
pub mod execution {
    include!(concat!(env!("OUT_DIR"), "/phoxal.execution.v1.rs"));
}

/// Public simulation authority and boundary protocol.
pub mod simulation {
    include!(concat!(env!("OUT_DIR"), "/phoxal.simulation.v1.rs"));
}

pub mod route;
pub mod validation;
pub use route::{PublicOperation, PublicRoute, PublicRouteKind, RouteError};
pub use validation::{
    BootstrapError, DeploymentTarget, MAX_BOOTSTRAP_BYTES, MAX_KEY_PREFIX_BYTES,
    MAX_PROTOCOL_BYTES, MAX_SESSION_OFFERS, SESSION_PROTOCOL, validate_session_offers,
};

/// Original descriptors for independent clients and bounded inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));
