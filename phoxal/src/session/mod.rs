//! Application-neutral attachment to one running Phoxal execution.
//!
//! [`Session::connect`] resolves one configured endpoint to one execution,
//! completes the frozen `supervisor/connect` bootstrap, refuses a peer built
//! from another compatibility line, and opens the current typed contracts. The
//! unique [`Session`] owns the transport lifetime and deterministic close;
//! cloneable [`SessionHandle`] values perform typed operations and cannot
//! create, replace, or close the session.
//!
//! Operations are named for what a contract *means*, not for a transport verb,
//! and every one of them is generic over the endpoint type the api tree
//! declares:
//!
//! - [`state_view`](SessionHandle::state_view),
//!   [`sample_receiver`](SessionHandle::sample_receiver),
//!   [`event_receiver`](SessionHandle::event_receiver) and
//!   [`stream_receiver`](SessionHandle::stream_receiver) observe what an owner
//!   publishes;
//! - [`setpoint_publisher`](SessionHandle::setpoint_publisher) and
//!   [`stream_publisher`](SessionHandle::stream_publisher) send to an owner;
//! - [`querier`](SessionHandle::querier) is the bounded request/reply leg, at
//!   the framework's current default timeout.
//!
//! Each takes the **client** side of a topic, which is what the api tree hands
//! back from `.client()`: a session is an external client of every contract it
//! touches, and taking an owner-side topic is a compile error.
//!
//! There are no per-family wrapper modules here. A consumer names
//! [`crate::api`], [`crate::runtime::api`], [`crate::supervisor::api`],
//! [`crate::identity`] and [`crate::version`] directly, because those are the
//! canonical paths and a second spelling of them could only drift.
//!
//! Losing the supervisor identity, the snapshot stream, or an owner-owned bus
//! worker is terminal for the session. [`SessionHandle::disconnect_reason`]
//! retains the first structured cause. Retry, reconnect, endpoint selection,
//! and broader timeout budgets remain decisions for the application.

mod connection;
mod error;

pub use crate::world::{
    WorldDiagnosticsSubscription, WorldSessionClient, WorldSessionWireError, WorldStateSubscription,
};
pub use connection::{ConnectOptions, ConnectedExecution, Session, SessionHandle};
pub use error::{CloseError, CompatibilityRefusal, ConnectError, DisconnectReason, SessionError};
