//! Backend-neutral world-session contracts and bounded loopback transport.

pub mod api;

mod local;

pub use local::{
    WorldDiagnosticsSubscription, WorldSessionClient, WorldSessionHandler, WorldSessionOperation,
    WorldSessionServer, WorldSessionWireError, WorldStateSubscription,
};
