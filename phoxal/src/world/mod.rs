//! Backend-neutral world-session contracts and bounded loopback transport.

pub mod api;

#[cfg(any(feature = "session", feature = "simulator", feature = "supervisor"))]
mod local;

#[cfg(any(feature = "session", feature = "simulator", feature = "supervisor"))]
pub use local::{
    WorldDiagnosticsSubscription, WorldSessionClient, WorldSessionHandler, WorldSessionOperation,
    WorldSessionServer, WorldSessionWireError, WorldStateSubscription,
};
