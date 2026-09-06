//! Bounded loopback MessagePack transport for the backend-neutral world API.
//!
//! This is deliberately separate from the execution bus. A world host owns no
//! `ExecutionId`; its registry record contains this loopback endpoint and the
//! frozen bootstrap below establishes the one `WorldInstanceId` it serves.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use std::{future::Future, pin::Pin};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};

use crate::bus::QueryEndpoint;
use crate::identity::ExecutionId;
use crate::model::identity::SpawnId;
use crate::version::FrameworkVersion;
use crate::world::api::session::WorldMemberPhase;
use crate::world::api::session::connect::{
    WorldSessionBootstrap, WorldSessionConnectRequest, WorldSessionConnectResponse,
};
use crate::world::api::session::control::{
    WorldControl, WorldSessionControlRequest, WorldSessionControlResponse,
};
use crate::world::api::session::diagnostics::{
    WorldSessionDiagnostics, WorldSessionDiagnosticsCurrentRequest,
    WorldSessionDiagnosticsCurrentResponse, WorldSessionDiagnosticsStream,
    WorldSessionDiagnosticsSubscriptionRequest,
};
use crate::world::api::session::state::{
    WorldSessionState, WorldSessionStateCurrentRequest, WorldSessionStateCurrentResponse,
    WorldSessionStateStream, WorldSessionStateSubscriptionRequest,
};

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 64;
const CLIENT_STREAM_CAPACITY: usize = 32;
#[cfg(not(test))]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
#[cfg(not(test))]
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(test)]
const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const FRAME_IO_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
const FRAME_IO_TIMEOUT: Duration = Duration::from_millis(500);
#[cfg(not(test))]
const HOST_OPERATION_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(test)]
const HOST_OPERATION_TIMEOUT: Duration = Duration::from_millis(500);
#[cfg(not(test))]
const CLIENT_OPERATION_TIMEOUT: Duration = Duration::from_secs(50);
#[cfg(test)]
const CLIENT_OPERATION_TIMEOUT: Duration = Duration::from_millis(750);

const STATE_PATH: &str = "world/session/state";
const STATE_CURRENT_PATH: &str = "world/session/state/current";
const DIAGNOSTICS_PATH: &str = "world/session/diagnostics";
const DIAGNOSTICS_CURRENT_PATH: &str = "world/session/diagnostics/current";
const CONTROL_PATH: &str = "world/session/control";
const CONNECT_PATH: &str = "world/session/connect";

mod client;
mod error;
mod framing;
mod server;
mod subscription;

pub use client::WorldSessionClient;
pub use error::WorldSessionWireError;
pub use server::{WorldSessionHandler, WorldSessionOperation, WorldSessionServer};
pub use subscription::{WorldDiagnosticsSubscription, WorldStateSubscription};

use framing::{
    WireRequest, decode_body, open_subscription, parse_endpoint, read_frame, request, send_error,
    send_gap, send_timeout, send_value, with_timeout,
};
use subscription::{WireSubscription, validate_state_against};

#[cfg(test)]
mod tests;
