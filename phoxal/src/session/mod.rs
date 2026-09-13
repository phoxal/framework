//! Typed external access to routed Phoxal supervisors and their executions.
//!
//! [`crate::session::connect`] opens one bounded physical connection from a [`crate::session::ConnectionConfig`].
//! A [`crate::session::Connection`] can discover supervisors and open an independent logical [`crate::session::Supervisor`] session for each selected target.
//! [`crate::session::Supervisor::management`] exposes lifecycle information, [`crate::session::Supervisor::execution`] selects an exact execution, and [`crate::session::Supervisor::simulation`] accesses the separately authorized simulation contract.
//! An [`crate::session::Execution`] selects a [`crate::session::Service`], whose [`crate::session::Service::port`] method accepts generated service-owned port descriptors and returns only the operations valid for that port kind.
//! Closing one logical supervisor session does not close its sibling sessions; [`crate::session::Connection::close`] owns the shared transport lifetime.

#[path = "public_error.rs"]
mod error;
mod public;

pub use crate::communication_transport::{
    DiscoveryEvent, PrincipalPolicy, PublicSessionBackend, PublicSessionConfig,
    PublicSessionConnection, PublicSessionTransport, PublicSimulationBackend,
    PublicSimulationContext, PublicSubscription, PublicTlsCredentials, PublicTransportError,
    PublicTransportLimits, PublicTransportSecurity, SupervisorWatch,
};
pub use error::SessionError;
pub use public::{
    CommandHandle, CommandOutcome, Connection, ConnectionConfig, EventHandle, EventSubscription,
    Execution, Management, ReadHandle, ReadOutcome, SampleHandle, SampleSubscription, Service,
    Simulation, StateHandle, StateSubscription, StateSubscriptionItem, StreamHandle,
    StreamSubscription, SubscriptionItem, Supervisor, connect,
};
