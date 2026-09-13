//! The private Zenoh owner used by Runtime and supervisor processes.
//!
//! Generated Protobuf messages and public-session operations live in
//! [`crate::communication`] and [`crate::communication_transport`]. This
//! module owns only the execution-scoped session and the embedded router's
//! transport configuration.

pub(crate) mod error;
pub(crate) mod session;

pub(crate) use error::BusError;
pub(crate) use session::{BusConfig, BusHandle, BusOwner, BusTerminal};
