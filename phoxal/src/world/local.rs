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

use crate::identity::ExecutionId;
use crate::model::identity::SpawnId;
use crate::version::FrameworkVersion;
use crate::world::api::session::WorldMemberPhase;
use crate::world::api::session::connect::{
    WorldSessionBootstrap, WorldSessionConnectRequest, WorldSessionConnectResponse,
};
use crate::world::api::session::control::{
    WorldSessionControlRequest, WorldSessionControlResponse,
};
use crate::world::api::session::diagnostics::{
    WorldSessionDiagnostics, WorldSessionDiagnosticsCurrentRequest,
    WorldSessionDiagnosticsCurrentResponse, WorldSessionDiagnosticsStream,
};
use crate::world::api::session::state::{
    WorldSessionState, WorldSessionStateCurrentRequest, WorldSessionStateCurrentResponse,
    WorldSessionStateStream,
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

/// One host operation whose completion is driven asynchronously by the host.
///
/// Attachment may perform supervisor queries, controller readiness
/// coordination, native simulator mutation, and rollback. Keeping that work
/// asynchronous prevents a client connection from blocking the Tokio worker
/// that serves the local session endpoint.
pub type WorldSessionOperation<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// Host-owned state and operation hooks served by [`WorldSessionServer`].
///
/// Implementations must make `state` and `subscribe_state` one serialized
/// authority, and likewise for diagnostics. The server subscribes before it
/// reads current, then filters buffered revisions, closing both races.
pub trait WorldSessionHandler: Send + Sync + 'static {
    fn bootstrap(&self) -> WorldSessionBootstrap;
    fn state(&self) -> WorldSessionState;
    fn subscribe_state(&self) -> broadcast::Receiver<WorldSessionState>;
    fn diagnostics(&self) -> WorldSessionDiagnostics;
    fn subscribe_diagnostics(&self) -> broadcast::Receiver<WorldSessionDiagnostics>;
    fn control(
        &self,
        request: WorldSessionControlRequest,
    ) -> WorldSessionOperation<'_, WorldSessionState>;
    fn attach(
        &self,
        execution: ExecutionId,
        supervisor_endpoint: String,
        spawn: Option<SpawnId>,
    ) -> WorldSessionOperation<'_, WorldSessionState>;
}

/// The unique listener for one local world session.
pub struct WorldSessionServer {
    endpoint: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), WorldSessionWireError>>>,
}

impl WorldSessionServer {
    /// Bind a private loopback port and start serving one host authority.
    pub async fn bind<H: WorldSessionHandler>(
        handler: Arc<H>,
    ) -> Result<Self, WorldSessionWireError> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let endpoint = format!("tcp://{address}");
        let (shutdown, stop) = oneshot::channel();
        let task = tokio::spawn(serve(listener, handler, stop));
        Ok(Self {
            endpoint,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn close(mut self) -> Result<(), WorldSessionWireError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        task.await
            .map_err(|error| WorldSessionWireError::Protocol(error.to_string()))?
    }
}

impl Drop for WorldSessionServer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// A verified client of one local world host.
#[derive(Clone, Debug)]
pub struct WorldSessionClient {
    endpoint: SocketAddr,
    bootstrap: WorldSessionBootstrap,
}

impl WorldSessionClient {
    /// Verify the frozen host bootstrap before trusting the registered endpoint.
    pub async fn connect(endpoint: &str) -> Result<Self, WorldSessionWireError> {
        let endpoint = parse_endpoint(endpoint)?;
        let response: WorldSessionConnectResponse = request(
            endpoint,
            CONNECT_PATH,
            &WorldSessionConnectRequest::Bootstrap {
                framework: FrameworkVersion::CURRENT,
            },
        )
        .await?;
        let WorldSessionConnectResponse::Bootstrap { bootstrap } = response else {
            return Err(WorldSessionWireError::Protocol(
                "world host returned an attachment response to bootstrap".to_owned(),
            ));
        };
        if !bootstrap
            .framework
            .is_compatible_with(FrameworkVersion::CURRENT)
        {
            return Err(WorldSessionWireError::IncompatibleFramework {
                local: FrameworkVersion::CURRENT,
                remote: bootstrap.framework,
            });
        }
        Ok(Self {
            endpoint,
            bootstrap,
        })
    }

    #[must_use]
    pub fn bootstrap(&self) -> &WorldSessionBootstrap {
        &self.bootstrap
    }

    pub async fn current_state(&self) -> Result<WorldSessionState, WorldSessionWireError> {
        let response: WorldSessionStateCurrentResponse = request(
            self.endpoint,
            STATE_CURRENT_PATH,
            &WorldSessionStateCurrentRequest {},
        )
        .await?;
        validate_state_against(&self.bootstrap, &response.state)?;
        Ok(response.state)
    }

    pub async fn state_subscription(
        &self,
    ) -> Result<WorldStateSubscription, WorldSessionWireError> {
        let updates =
            open_subscription::<WorldSessionStateStream>(self.endpoint, STATE_PATH).await?;
        let current = self.current_state().await?;
        WorldStateSubscription::reconcile(self.bootstrap.clone(), current, updates)
    }

    pub async fn current_diagnostics(
        &self,
    ) -> Result<WorldSessionDiagnostics, WorldSessionWireError> {
        let response: WorldSessionDiagnosticsCurrentResponse = request(
            self.endpoint,
            DIAGNOSTICS_CURRENT_PATH,
            &WorldSessionDiagnosticsCurrentRequest {},
        )
        .await?;
        response.diagnostics.validate()?;
        Ok(response.diagnostics)
    }

    pub async fn diagnostics_subscription(
        &self,
    ) -> Result<WorldDiagnosticsSubscription, WorldSessionWireError> {
        let updates =
            open_subscription::<WorldSessionDiagnosticsStream>(self.endpoint, DIAGNOSTICS_PATH)
                .await?;
        let current = self.current_diagnostics().await?;
        WorldDiagnosticsSubscription::reconcile(current, updates)
    }

    pub async fn control(
        &self,
        operation: WorldSessionControlRequest,
    ) -> Result<WorldSessionState, WorldSessionWireError> {
        let response: WorldSessionControlResponse =
            request(self.endpoint, CONTROL_PATH, &operation).await?;
        validate_state_against(&self.bootstrap, &response.state)?;
        Ok(response.state)
    }

    pub async fn attach(
        &self,
        execution: ExecutionId,
        supervisor_endpoint: impl Into<String>,
        spawn: Option<SpawnId>,
    ) -> Result<WorldSessionState, WorldSessionWireError> {
        let response: WorldSessionConnectResponse = request(
            self.endpoint,
            CONNECT_PATH,
            &WorldSessionConnectRequest::Attach {
                framework: FrameworkVersion::CURRENT,
                execution,
                supervisor_endpoint: supervisor_endpoint.into(),
                spawn,
            },
        )
        .await?;
        let WorldSessionConnectResponse::Attached { state } = response else {
            return Err(WorldSessionWireError::Protocol(
                "world host returned a bootstrap response to attachment".to_owned(),
            ));
        };
        validate_state_against(&self.bootstrap, &state)?;
        Ok(state)
    }
}

/// A gap-free current state plus every strictly newer complete replacement.
pub struct WorldStateSubscription {
    bootstrap: WorldSessionBootstrap,
    current: WorldSessionState,
    updates: WireSubscription<WorldSessionStateStream>,
    last_stream_revision: u64,
    last_stream_progress: crate::model::world::WorldProgress,
}

impl WorldStateSubscription {
    fn reconcile(
        bootstrap: WorldSessionBootstrap,
        mut current: WorldSessionState,
        mut updates: WireSubscription<WorldSessionStateStream>,
    ) -> Result<Self, WorldSessionWireError> {
        let mut last_stream_revision = None;
        let mut last_stream_progress = None;
        while let Some(update) = updates.try_recv()? {
            validate_state_against(&bootstrap, &update.state)?;
            validate_stream_revision("state", &mut last_stream_revision, update.state.revision)?;
            validate_stream_progress(&mut last_stream_progress, update.state.progress)?;
            if update.state.revision > current.revision {
                validate_progress_not_before(current.progress, update.state.progress)?;
                current = update.state;
            }
        }
        let last_stream_revision = last_stream_revision.ok_or_else(|| {
            WorldSessionWireError::Protocol(
                "world state subscription did not begin with a snapshot".to_owned(),
            )
        })?;
        let last_stream_progress = last_stream_progress.ok_or_else(|| {
            WorldSessionWireError::Protocol(
                "world state subscription did not begin with progress".to_owned(),
            )
        })?;
        Ok(Self {
            bootstrap,
            current,
            updates,
            last_stream_revision,
            last_stream_progress,
        })
    }

    #[must_use]
    pub fn current(&self) -> &WorldSessionState {
        &self.current
    }

    pub fn try_recv(&mut self) -> Result<Option<&WorldSessionState>, WorldSessionWireError> {
        let Some(update) = self.updates.try_recv()? else {
            return Ok(None);
        };
        validate_state_against(&self.bootstrap, &update.state)?;
        validate_stream_revision(
            "state",
            &mut Some(self.last_stream_revision),
            update.state.revision,
        )?;
        validate_progress_not_before(self.last_stream_progress, update.state.progress)?;
        self.last_stream_revision = update.state.revision;
        self.last_stream_progress = update.state.progress;
        if update.state.revision <= self.current.revision {
            return Ok(None);
        }
        validate_progress_not_before(self.current.progress, update.state.progress)?;
        self.current = update.state;
        Ok(Some(&self.current))
    }

    pub async fn recv(&mut self) -> Result<&WorldSessionState, WorldSessionWireError> {
        loop {
            let update = self.updates.recv().await?;
            validate_state_against(&self.bootstrap, &update.state)?;
            validate_stream_revision(
                "state",
                &mut Some(self.last_stream_revision),
                update.state.revision,
            )?;
            validate_progress_not_before(self.last_stream_progress, update.state.progress)?;
            self.last_stream_revision = update.state.revision;
            self.last_stream_progress = update.state.progress;
            if update.state.revision > self.current.revision {
                validate_progress_not_before(self.current.progress, update.state.progress)?;
                self.current = update.state;
                return Ok(&self.current);
            }
        }
    }

    pub async fn wait_for_member_active(
        &mut self,
        execution: ExecutionId,
    ) -> Result<&WorldSessionState, WorldSessionWireError> {
        loop {
            if self.current.members.iter().any(|member| {
                member.execution == execution && member.phase == WorldMemberPhase::Active
            }) {
                return Ok(&self.current);
            }
            self.recv().await?;
        }
    }
}

/// A gap-free current diagnostics value plus strictly newer replacements.
pub struct WorldDiagnosticsSubscription {
    current: WorldSessionDiagnostics,
    updates: WireSubscription<WorldSessionDiagnosticsStream>,
    last_stream_revision: u64,
}

impl WorldDiagnosticsSubscription {
    fn reconcile(
        mut current: WorldSessionDiagnostics,
        mut updates: WireSubscription<WorldSessionDiagnosticsStream>,
    ) -> Result<Self, WorldSessionWireError> {
        current.validate()?;
        let mut last_stream_revision = None;
        while let Some(update) = updates.try_recv()? {
            update.diagnostics.validate()?;
            validate_stream_revision(
                "diagnostics",
                &mut last_stream_revision,
                update.diagnostics.revision,
            )?;
            if update.diagnostics.revision > current.revision {
                current = update.diagnostics;
            }
        }
        let last_stream_revision = last_stream_revision.ok_or_else(|| {
            WorldSessionWireError::Protocol(
                "world diagnostics subscription did not begin with a snapshot".to_owned(),
            )
        })?;
        Ok(Self {
            current,
            updates,
            last_stream_revision,
        })
    }

    #[must_use]
    pub const fn current(&self) -> WorldSessionDiagnostics {
        self.current
    }

    pub fn try_recv(&mut self) -> Result<Option<WorldSessionDiagnostics>, WorldSessionWireError> {
        let Some(update) = self.updates.try_recv()? else {
            return Ok(None);
        };
        update.diagnostics.validate()?;
        validate_stream_revision(
            "diagnostics",
            &mut Some(self.last_stream_revision),
            update.diagnostics.revision,
        )?;
        self.last_stream_revision = update.diagnostics.revision;
        if update.diagnostics.revision <= self.current.revision {
            return Ok(None);
        }
        self.current = update.diagnostics;
        Ok(Some(self.current))
    }

    pub async fn recv(&mut self) -> Result<WorldSessionDiagnostics, WorldSessionWireError> {
        loop {
            let update = self.updates.recv().await?;
            update.diagnostics.validate()?;
            validate_stream_revision(
                "diagnostics",
                &mut Some(self.last_stream_revision),
                update.diagnostics.revision,
            )?;
            self.last_stream_revision = update.diagnostics.revision;
            if update.diagnostics.revision > self.current.revision {
                self.current = update.diagnostics;
                return Ok(self.current);
            }
        }
    }
}

struct WireSubscription<T> {
    receiver: mpsc::Receiver<Result<T, WorldSessionWireError>>,
    task: JoinHandle<()>,
}

impl<T> WireSubscription<T> {
    fn try_recv(&mut self) -> Result<Option<T>, WorldSessionWireError> {
        match self.receiver.try_recv() {
            Ok(Ok(value)) => Ok(Some(value)),
            Ok(Err(error)) => Err(error),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => Err(WorldSessionWireError::Closed),
        }
    }

    async fn recv(&mut self) -> Result<T, WorldSessionWireError> {
        self.receiver
            .recv()
            .await
            .ok_or(WorldSessionWireError::Closed)?
    }
}

impl<T> Drop for WireSubscription<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorldSessionWireError {
    #[error("world-session I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("world-session encoding failed: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("world-session decoding failed: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("world-session frame is {bytes} bytes, exceeding the {maximum}-byte bound")]
    FrameTooLarge { bytes: usize, maximum: usize },
    #[error("invalid loopback world-session endpoint '{endpoint}'")]
    InvalidEndpoint { endpoint: String },
    #[error("remote framework {remote} is incompatible with local framework {local}")]
    IncompatibleFramework {
        local: FrameworkVersion,
        remote: FrameworkVersion,
    },
    #[error("world host refused the operation: {0}")]
    Refused(String),
    #[error("invalid world-session state: {0}")]
    State(#[from] crate::world::api::session::state::WorldSessionStateError),
    #[error("invalid world-session diagnostics: {0}")]
    Diagnostics(#[from] crate::world::api::session::diagnostics::WorldSessionDiagnosticsError),
    #[error("world-session {operation} timed out after {timeout_ms} ms")]
    Timeout { operation: String, timeout_ms: u64 },
    #[error("world-session protocol failed: {0}")]
    Protocol(String),
    #[error("world-session state contradicts frozen bootstrap field '{field}'")]
    BootstrapMismatch { field: &'static str },
    #[error("the world-session stream closed")]
    Closed,
    #[error(
        "the world-session {stream} stream lost {skipped} replacement(s); query current and resubscribe"
    )]
    StreamGap { stream: &'static str, skipped: u64 },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    path: String,
    body: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireReply {
    Value { body: Vec<u8> },
    Error { message: String },
    Timeout { operation: String, timeout_ms: u64 },
    Gap { stream: String, skipped: u64 },
}

async fn serve<H: WorldSessionHandler>(
    listener: TcpListener,
    handler: Arc<H>,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<(), WorldSessionWireError> {
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                connections.shutdown().await;
                return Ok(());
            }
            accepted = listener.accept() => {
                let (stream, address) = accepted?;
                if !address.ip().is_loopback() {
                    continue;
                }
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    continue;
                };
                let handler = Arc::clone(&handler);
                connections.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = serve_connection(stream, handler).await {
                        tracing::warn!(target: "phoxal.world.session", %error, "world-session client ended with an error");
                    }
                });
            }
            Some(joined) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = joined {
                    tracing::warn!(target: "phoxal.world.session", %error, "world-session connection task failed");
                }
            }
        }
    }
}

async fn serve_connection<H: WorldSessionHandler>(
    mut stream: TcpStream,
    handler: Arc<H>,
) -> Result<(), WorldSessionWireError> {
    let request: WireRequest = with_timeout(
        "server handshake",
        HANDSHAKE_TIMEOUT,
        read_frame(&mut stream),
    )
    .await?;
    match request.path.as_str() {
        STATE_PATH => serve_state_stream(&mut stream, handler).await,
        STATE_CURRENT_PATH => {
            decode_body::<WorldSessionStateCurrentRequest>(&request.body)?;
            send_value(
                &mut stream,
                &WorldSessionStateCurrentResponse {
                    state: handler.state(),
                },
            )
            .await
        }
        DIAGNOSTICS_PATH => serve_diagnostics_stream(&mut stream, handler).await,
        DIAGNOSTICS_CURRENT_PATH => {
            decode_body::<WorldSessionDiagnosticsCurrentRequest>(&request.body)?;
            send_value(
                &mut stream,
                &WorldSessionDiagnosticsCurrentResponse {
                    diagnostics: handler.diagnostics(),
                },
            )
            .await
        }
        CONTROL_PATH => {
            let control = decode_body(&request.body)?;
            match tokio::time::timeout(HOST_OPERATION_TIMEOUT, handler.control(control)).await {
                Ok(Ok(state)) => {
                    send_value(&mut stream, &WorldSessionControlResponse { state }).await
                }
                Ok(Err(message)) => send_error(&mut stream, message).await,
                Err(_) => send_timeout(&mut stream, "host control", HOST_OPERATION_TIMEOUT).await,
            }
        }
        CONNECT_PATH => serve_connect(&mut stream, handler, &request.body).await,
        _ => {
            send_error(
                &mut stream,
                format!("unknown world-session path '{}'", request.path),
            )
            .await
        }
    }
}

async fn serve_connect<H: WorldSessionHandler>(
    stream: &mut TcpStream,
    handler: Arc<H>,
    body: &[u8],
) -> Result<(), WorldSessionWireError> {
    match decode_body::<WorldSessionConnectRequest>(body)? {
        WorldSessionConnectRequest::Bootstrap { .. } => {
            send_value(
                stream,
                &WorldSessionConnectResponse::Bootstrap {
                    bootstrap: handler.bootstrap(),
                },
            )
            .await
        }
        WorldSessionConnectRequest::Attach {
            framework,
            execution,
            supervisor_endpoint,
            spawn,
        } => {
            if !framework.is_compatible_with(FrameworkVersion::CURRENT) {
                return send_error(
                    stream,
                    format!(
                        "framework {framework} is incompatible with host {}",
                        FrameworkVersion::CURRENT
                    ),
                )
                .await;
            }
            match tokio::time::timeout(
                HOST_OPERATION_TIMEOUT,
                handler.attach(execution, supervisor_endpoint, spawn),
            )
            .await
            {
                Ok(Ok(state)) => {
                    send_value(stream, &WorldSessionConnectResponse::Attached { state }).await
                }
                Ok(Err(message)) => send_error(stream, message).await,
                Err(_) => send_timeout(stream, "host attachment", HOST_OPERATION_TIMEOUT).await,
            }
        }
    }
}

async fn serve_state_stream<H: WorldSessionHandler>(
    stream: &mut TcpStream,
    handler: Arc<H>,
) -> Result<(), WorldSessionWireError> {
    let mut updates = handler.subscribe_state();
    let current = handler.state();
    let mut revision = current.revision;
    send_value(stream, &WorldSessionStateStream { state: current }).await?;
    loop {
        match updates.try_recv() {
            Ok(state) if state.revision > revision => {
                revision = state.revision;
                send_value(stream, &WorldSessionStateStream { state }).await?;
            }
            Ok(_) => continue,
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                send_gap(stream, "state", skipped).await?;
                return Ok(());
            }
            Err(broadcast::error::TryRecvError::Closed) => return Ok(()),
        }
    }
    loop {
        match updates.recv().await {
            Ok(state) if state.revision > revision => {
                revision = state.revision;
                send_value(stream, &WorldSessionStateStream { state }).await?;
            }
            Ok(state) => {
                send_error(
                    stream,
                    format!(
                        "world state revision {} did not increase beyond {revision}",
                        state.revision
                    ),
                )
                .await?;
                return Ok(());
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                send_gap(stream, "state", skipped).await?;
                return Ok(());
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

async fn serve_diagnostics_stream<H: WorldSessionHandler>(
    stream: &mut TcpStream,
    handler: Arc<H>,
) -> Result<(), WorldSessionWireError> {
    let mut updates = handler.subscribe_diagnostics();
    let current = handler.diagnostics();
    let mut revision = current.revision;
    send_value(
        stream,
        &WorldSessionDiagnosticsStream {
            diagnostics: current,
        },
    )
    .await?;
    loop {
        match updates.try_recv() {
            Ok(diagnostics) if diagnostics.revision > revision => {
                revision = diagnostics.revision;
                send_value(stream, &WorldSessionDiagnosticsStream { diagnostics }).await?;
            }
            Ok(_) => continue,
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                send_gap(stream, "diagnostics", skipped).await?;
                return Ok(());
            }
            Err(broadcast::error::TryRecvError::Closed) => return Ok(()),
        }
    }
    loop {
        match updates.recv().await {
            Ok(diagnostics) if diagnostics.revision > revision => {
                revision = diagnostics.revision;
                send_value(stream, &WorldSessionDiagnosticsStream { diagnostics }).await?;
            }
            Ok(diagnostics) => {
                send_error(
                    stream,
                    format!(
                        "world diagnostics revision {} did not increase beyond {revision}",
                        diagnostics.revision
                    ),
                )
                .await?;
                return Ok(());
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                send_gap(stream, "diagnostics", skipped).await?;
                return Ok(());
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

async fn with_timeout<T, F>(
    operation: &'static str,
    timeout: Duration,
    future: F,
) -> Result<T, WorldSessionWireError>
where
    F: Future<Output = Result<T, WorldSessionWireError>>,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| WorldSessionWireError::Timeout {
            operation: operation.to_owned(),
            timeout_ms: timeout_millis(timeout),
        })?
}

fn timeout_millis(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
}

async fn request<Req: Serialize, Resp: DeserializeOwned>(
    endpoint: SocketAddr,
    path: &str,
    request: &Req,
) -> Result<Resp, WorldSessionWireError> {
    let mut stream = with_timeout("connect", CONNECT_TIMEOUT, async move {
        Ok(TcpStream::connect(endpoint).await?)
    })
    .await?;
    with_timeout(
        "request write",
        FRAME_IO_TIMEOUT,
        write_frame(
            &mut stream,
            &WireRequest {
                path: path.to_owned(),
                body: rmp_serde::to_vec_named(request)?,
            },
        ),
    )
    .await?;
    let reply = with_timeout(
        "request response",
        CLIENT_OPERATION_TIMEOUT,
        read_frame(&mut stream),
    )
    .await?;
    decode_reply(reply)
}

async fn open_subscription<T: DeserializeOwned + Send + 'static>(
    endpoint: SocketAddr,
    path: &str,
) -> Result<WireSubscription<T>, WorldSessionWireError> {
    let mut stream = with_timeout("connect", CONNECT_TIMEOUT, async move {
        Ok(TcpStream::connect(endpoint).await?)
    })
    .await?;
    with_timeout(
        "subscription request write",
        FRAME_IO_TIMEOUT,
        write_frame(
            &mut stream,
            &WireRequest {
                path: path.to_owned(),
                body: Vec::new(),
            },
        ),
    )
    .await?;
    let reply = with_timeout(
        "subscription handshake",
        FRAME_IO_TIMEOUT,
        read_frame::<_, WireReply>(&mut stream),
    )
    .await?;
    let initial = decode_reply(reply)?;
    let (sender, receiver) = mpsc::channel(CLIENT_STREAM_CAPACITY);
    sender.try_send(Ok(initial)).map_err(|_| {
        WorldSessionWireError::Protocol("subscription bootstrap queue closed".to_owned())
    })?;
    let task = tokio::spawn(async move {
        loop {
            let value = match read_frame::<_, WireReply>(&mut stream).await {
                Ok(reply) => decode_reply(reply),
                Err(error) => Err(error),
            };
            let terminal = value.is_err();
            if sender.send(value).await.is_err() || terminal {
                return;
            }
        }
    });
    Ok(WireSubscription { receiver, task })
}

async fn send_value<T: Serialize>(
    stream: &mut TcpStream,
    value: &T,
) -> Result<(), WorldSessionWireError> {
    with_timeout(
        "response write",
        FRAME_IO_TIMEOUT,
        write_frame(
            stream,
            &WireReply::Value {
                body: rmp_serde::to_vec_named(value)?,
            },
        ),
    )
    .await
}

async fn send_error(stream: &mut TcpStream, message: String) -> Result<(), WorldSessionWireError> {
    with_timeout(
        "error response write",
        FRAME_IO_TIMEOUT,
        write_frame(stream, &WireReply::Error { message }),
    )
    .await
}

async fn send_timeout(
    stream: &mut TcpStream,
    operation: &'static str,
    timeout: Duration,
) -> Result<(), WorldSessionWireError> {
    with_timeout(
        "timeout response write",
        FRAME_IO_TIMEOUT,
        write_frame(
            stream,
            &WireReply::Timeout {
                operation: operation.to_owned(),
                timeout_ms: timeout_millis(timeout),
            },
        ),
    )
    .await
}

async fn send_gap(
    stream: &mut TcpStream,
    stream_name: &'static str,
    skipped: u64,
) -> Result<(), WorldSessionWireError> {
    with_timeout(
        "stream gap response write",
        FRAME_IO_TIMEOUT,
        write_frame(
            stream,
            &WireReply::Gap {
                stream: stream_name.to_owned(),
                skipped,
            },
        ),
    )
    .await
}

fn decode_reply<T: DeserializeOwned>(reply: WireReply) -> Result<T, WorldSessionWireError> {
    match reply {
        WireReply::Value { body } => Ok(rmp_serde::from_slice(&body)?),
        WireReply::Error { message } => Err(WorldSessionWireError::Refused(message)),
        WireReply::Timeout {
            operation,
            timeout_ms,
        } => Err(WorldSessionWireError::Timeout {
            operation,
            timeout_ms,
        }),
        WireReply::Gap { stream, skipped } => {
            let stream = match stream.as_str() {
                "state" => "state",
                "diagnostics" => "diagnostics",
                _ => {
                    return Err(WorldSessionWireError::Protocol(format!(
                        "world host reported a gap for unknown stream '{stream}'"
                    )));
                }
            };
            Err(WorldSessionWireError::StreamGap { stream, skipped })
        }
    }
}

fn validate_stream_revision(
    stream: &'static str,
    previous: &mut Option<u64>,
    revision: u64,
) -> Result<(), WorldSessionWireError> {
    if let Some(previous) = *previous
        && revision <= previous
    {
        return Err(WorldSessionWireError::Protocol(format!(
            "world {stream} revision {revision} did not increase beyond {previous}"
        )));
    }
    *previous = Some(revision);
    Ok(())
}

fn validate_state_against(
    bootstrap: &WorldSessionBootstrap,
    state: &WorldSessionState,
) -> Result<(), WorldSessionWireError> {
    state.validate()?;
    if state.instance != bootstrap.instance {
        return Err(WorldSessionWireError::BootstrapMismatch { field: "instance" });
    }
    if state.provenance.world != bootstrap.world {
        return Err(WorldSessionWireError::BootstrapMismatch { field: "world" });
    }
    if state.provenance.digest != bootstrap.digest {
        return Err(WorldSessionWireError::BootstrapMismatch { field: "digest" });
    }
    if state.provenance.framework != bootstrap.framework {
        return Err(WorldSessionWireError::BootstrapMismatch { field: "framework" });
    }
    Ok(())
}

fn validate_stream_progress(
    previous: &mut Option<crate::model::world::WorldProgress>,
    progress: crate::model::world::WorldProgress,
) -> Result<(), WorldSessionWireError> {
    if let Some(previous) = *previous {
        validate_progress_not_before(previous, progress)?;
    }
    *previous = Some(progress);
    Ok(())
}

fn validate_progress_not_before(
    previous: crate::model::world::WorldProgress,
    observed: crate::model::world::WorldProgress,
) -> Result<(), WorldSessionWireError> {
    if observed.completed_step() < previous.completed_step()
        || observed.elapsed_ns() < previous.elapsed_ns()
    {
        return Err(WorldSessionWireError::Protocol(format!(
            "world progress regressed from step {} at {} ns to step {} at {} ns",
            previous.completed_step(),
            previous.elapsed_ns(),
            observed.completed_step(),
            observed.elapsed_ns(),
        )));
    }
    Ok(())
}

fn decode_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, WorldSessionWireError> {
    Ok(rmp_serde::from_slice(body)?)
}

async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), WorldSessionWireError> {
    let body = rmp_serde::to_vec_named(value)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(WorldSessionWireError::FrameTooLarge {
            bytes: body.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    let length = u32::try_from(body.len()).map_err(|_| WorldSessionWireError::FrameTooLarge {
        bytes: body.len(),
        maximum: MAX_FRAME_BYTES,
    })?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<T, WorldSessionWireError> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(WorldSessionWireError::FrameTooLarge {
            bytes: length,
            maximum: MAX_FRAME_BYTES,
        });
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).await?;
    Ok(rmp_serde::from_slice(&body)?)
}

fn parse_endpoint(endpoint: &str) -> Result<SocketAddr, WorldSessionWireError> {
    let address =
        endpoint
            .strip_prefix("tcp://")
            .ok_or_else(|| WorldSessionWireError::InvalidEndpoint {
                endpoint: endpoint.to_owned(),
            })?;
    let address =
        address
            .parse::<SocketAddr>()
            .map_err(|_| WorldSessionWireError::InvalidEndpoint {
                endpoint: endpoint.to_owned(),
            })?;
    if !address.ip().is_loopback() {
        return Err(WorldSessionWireError::InvalidEndpoint {
            endpoint: endpoint.to_owned(),
        });
    }
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::identity::WorldId;
    use crate::model::world::{WorldDigest, WorldInstanceId, WorldProgress, WorldProvenance};
    use crate::world::api::session::diagnostics::ObservedWorldPacing;
    use crate::world::api::session::{WorldLifecycle, WorldMotion};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn compatible_patch_other_than_current() -> FrameworkVersion {
        let current = FrameworkVersion::CURRENT;
        let patch = if current.patch() == u16::MAX {
            current.patch() - 1
        } else {
            current.patch() + 1
        };
        FrameworkVersion::new(current.major(), current.minor(), patch)
    }

    struct TestHandler {
        bootstrap: WorldSessionBootstrap,
        state: std::sync::Mutex<WorldSessionState>,
        states: broadcast::Sender<WorldSessionState>,
        diagnostics: std::sync::Mutex<WorldSessionDiagnostics>,
        diagnostic_updates: broadcast::Sender<WorldSessionDiagnostics>,
        race_state_subscription: AtomicBool,
        race_diagnostics_subscription: AtomicBool,
        hang_attach: bool,
    }

    impl TestHandler {
        fn new() -> Self {
            let instance = WorldInstanceId::mint();
            let world = WorldId::new("warehouse").expect("a valid world id");
            let digest = WorldDigest::parse(&"00".repeat(32)).expect("a canonical digest");
            let bootstrap = WorldSessionBootstrap {
                instance,
                framework: FrameworkVersion::CURRENT,
                world: world.clone(),
                digest,
            };
            let state = WorldSessionState {
                revision: 0,
                instance,
                provenance: WorldProvenance {
                    world,
                    digest,
                    random_seed: 0,
                    framework: FrameworkVersion::CURRENT,
                    adapter: "test".to_owned(),
                    adapter_version: "1".to_owned(),
                    simulator_version: "1".to_owned(),
                    platform: "test".to_owned(),
                    time_step_ns: 12,
                },
                lifecycle: WorldLifecycle::Ready {
                    motion: WorldMotion::Paused,
                },
                progress: WorldProgress::zero(12).expect("valid zero progress"),
                members: Vec::new(),
            };
            let (states, _) = broadcast::channel(8);
            let diagnostics = WorldSessionDiagnostics {
                revision: 0,
                pacing: None,
                last_transition_age_ns: None,
            };
            let (diagnostic_updates, _) = broadcast::channel(8);
            Self {
                bootstrap,
                state: std::sync::Mutex::new(state),
                states,
                diagnostics: std::sync::Mutex::new(diagnostics),
                diagnostic_updates,
                race_state_subscription: AtomicBool::new(false),
                race_diagnostics_subscription: AtomicBool::new(false),
                hang_attach: false,
            }
        }

        fn with_subscription_races(mut self) -> Self {
            self.race_state_subscription = AtomicBool::new(true);
            self.race_diagnostics_subscription = AtomicBool::new(true);
            self
        }

        fn with_hanging_attach(mut self) -> Self {
            self.hang_attach = true;
            self
        }

        fn replace_motion(&self, motion: WorldMotion) -> WorldSessionState {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.lifecycle != (WorldLifecycle::Ready { motion }) {
                state.revision += 1;
                state.lifecycle = WorldLifecycle::Ready { motion };
                let _ = self.states.send(state.clone());
            }
            state.clone()
        }

        fn replace_diagnostics(
            &self,
            pacing: Option<ObservedWorldPacing>,
        ) -> WorldSessionDiagnostics {
            let mut diagnostics = self
                .diagnostics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            diagnostics.revision += 1;
            diagnostics.pacing = pacing;
            diagnostics.last_transition_age_ns = Some(diagnostics.revision);
            let _ = self.diagnostic_updates.send(*diagnostics);
            *diagnostics
        }
    }

    impl WorldSessionHandler for TestHandler {
        fn bootstrap(&self) -> WorldSessionBootstrap {
            self.bootstrap.clone()
        }

        fn state(&self) -> WorldSessionState {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        fn subscribe_state(&self) -> broadcast::Receiver<WorldSessionState> {
            let updates = self.states.subscribe();
            if self.race_state_subscription.swap(false, Ordering::AcqRel) {
                self.replace_motion(WorldMotion::Running);
            }
            updates
        }

        fn diagnostics(&self) -> WorldSessionDiagnostics {
            *self
                .diagnostics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn subscribe_diagnostics(&self) -> broadcast::Receiver<WorldSessionDiagnostics> {
            let updates = self.diagnostic_updates.subscribe();
            if self
                .race_diagnostics_subscription
                .swap(false, Ordering::AcqRel)
            {
                self.replace_diagnostics(None);
            }
            updates
        }

        fn control(
            &self,
            request: WorldSessionControlRequest,
        ) -> WorldSessionOperation<'_, WorldSessionState> {
            Box::pin(async move {
                Ok(match request {
                    WorldSessionControlRequest::Pause => self.replace_motion(WorldMotion::Paused),
                    WorldSessionControlRequest::Resume => self.replace_motion(WorldMotion::Running),
                    WorldSessionControlRequest::Stop => {
                        let mut state = self
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if state.lifecycle != WorldLifecycle::Stopping {
                            state.revision += 1;
                            state.lifecycle = WorldLifecycle::Stopping;
                            let _ = self.states.send(state.clone());
                        }
                        state.clone()
                    }
                })
            })
        }

        fn attach(
            &self,
            _execution: ExecutionId,
            _supervisor_endpoint: String,
            _spawn: Option<SpawnId>,
        ) -> WorldSessionOperation<'_, WorldSessionState> {
            Box::pin(async move {
                if self.hang_attach {
                    std::future::pending::<()>().await;
                }
                Ok(self.state())
            })
        }
    }

    #[tokio::test]
    async fn loopback_client_reconciles_and_drives_idempotent_operations() {
        let handler = Arc::new(TestHandler::new());
        let server = WorldSessionServer::bind(Arc::clone(&handler))
            .await
            .expect("the loopback server binds");
        let client = WorldSessionClient::connect(server.endpoint())
            .await
            .expect("the client verifies bootstrap");
        assert_eq!(client.bootstrap(), &handler.bootstrap);

        let mut states = client
            .state_subscription()
            .await
            .expect("subscribe-first state reconciliation succeeds");
        assert_eq!(states.current().revision, 0);
        let running = client
            .control(WorldSessionControlRequest::Resume)
            .await
            .expect("resume is accepted");
        assert_eq!(running.revision, 1);
        assert_eq!(
            states.recv().await.expect("the replacement is delivered"),
            &running
        );
        let retry = client
            .control(WorldSessionControlRequest::Resume)
            .await
            .expect("resume retry is idempotent");
        assert_eq!(retry.revision, running.revision);

        let attached = client
            .attach(ExecutionId::mint(), "tcp/localhost:7447", None)
            .await
            .expect("the async host operation returns one complete state");
        assert_eq!(attached.revision, running.revision);
        assert_eq!(
            client
                .current_diagnostics()
                .await
                .expect("diagnostics current is available")
                .revision,
            0
        );

        drop(states);
        server.close().await.expect("the server closes cleanly");
    }

    #[tokio::test]
    async fn client_rejects_state_that_contradicts_frozen_bootstrap() {
        let handler = Arc::new(TestHandler::new());
        let server = WorldSessionServer::bind(Arc::clone(&handler))
            .await
            .expect("the loopback server binds");
        let client = WorldSessionClient::connect(server.endpoint())
            .await
            .expect("the client verifies bootstrap");
        handler
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .instance = WorldInstanceId::mint();

        assert!(matches!(
            client.current_state().await,
            Err(WorldSessionWireError::BootstrapMismatch { field: "instance" })
        ));
        server.close().await.expect("the server closes cleanly");
    }

    #[tokio::test]
    async fn attachment_preserves_the_frozen_instance_and_exact_framework_patch() {
        let handler = Arc::new(TestHandler::new());
        let server = WorldSessionServer::bind(Arc::clone(&handler))
            .await
            .expect("the loopback server binds");
        let client = WorldSessionClient::connect(server.endpoint())
            .await
            .expect("the client verifies bootstrap");
        let original_instance = handler.bootstrap.instance;
        {
            let mut state = handler
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.instance = WorldInstanceId::mint();
        }
        assert!(matches!(
            client
                .attach(ExecutionId::mint(), "tcp/localhost:7447", None)
                .await,
            Err(WorldSessionWireError::BootstrapMismatch { field: "instance" })
        ));

        let other_patch = compatible_patch_other_than_current();
        assert!(other_patch.is_compatible_with(FrameworkVersion::CURRENT));
        {
            let mut state = handler
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.instance = original_instance;
            state.provenance.framework = other_patch;
        }
        assert!(matches!(
            client
                .attach(ExecutionId::mint(), "tcp/localhost:7447", None)
                .await,
            Err(WorldSessionWireError::BootstrapMismatch { field: "framework" })
        ));

        server.close().await.expect("the server closes cleanly");
    }

    #[tokio::test]
    async fn streams_discard_subscribe_current_duplicates_and_remain_live() {
        let handler = Arc::new(TestHandler::new().with_subscription_races());
        let server = WorldSessionServer::bind(Arc::clone(&handler))
            .await
            .expect("the loopback server binds");
        let client = WorldSessionClient::connect(server.endpoint())
            .await
            .expect("the client verifies bootstrap");

        let mut states = client
            .state_subscription()
            .await
            .expect("the raced state snapshot reconciles");
        assert_eq!(states.current().revision, 1);
        let paused = handler.replace_motion(WorldMotion::Paused);
        assert_eq!(
            states.recv().await.expect("the state stream remains live"),
            &paused
        );

        let mut diagnostics = client
            .diagnostics_subscription()
            .await
            .expect("the raced diagnostics snapshot reconciles");
        assert_eq!(diagnostics.current().revision, 1);
        let next = handler.replace_diagnostics(Some(ObservedWorldPacing {
            world_elapsed_ns: 12,
            host_elapsed_ns: 20,
            completed_transitions: 1,
        }));
        assert_eq!(
            diagnostics
                .recv()
                .await
                .expect("the diagnostics stream remains live"),
            next
        );

        server.close().await.expect("the server closes cleanly");
    }

    #[tokio::test]
    async fn invalid_pacing_is_rejected_from_current_and_streamed_diagnostics() {
        let handler = Arc::new(TestHandler::new());
        let server = WorldSessionServer::bind(Arc::clone(&handler))
            .await
            .expect("the loopback server binds");
        let client = WorldSessionClient::connect(server.endpoint())
            .await
            .expect("the client verifies bootstrap");

        handler.replace_diagnostics(Some(ObservedWorldPacing {
            world_elapsed_ns: 0,
            host_elapsed_ns: 1,
            completed_transitions: 1,
        }));
        assert!(matches!(
            client.current_diagnostics().await,
            Err(WorldSessionWireError::Diagnostics(_))
        ));

        handler.replace_diagnostics(None);
        let mut diagnostics = client
            .diagnostics_subscription()
            .await
            .expect("valid diagnostics subscribe");
        handler.replace_diagnostics(Some(ObservedWorldPacing {
            world_elapsed_ns: 1,
            host_elapsed_ns: 0,
            completed_transitions: 1,
        }));
        assert!(matches!(
            diagnostics.recv().await,
            Err(WorldSessionWireError::Diagnostics(_))
        ));

        server.close().await.expect("the server closes cleanly");
    }

    #[tokio::test]
    async fn client_and_host_operations_have_typed_deadlines() {
        let silent_listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("the silent listener binds");
        let silent_address = silent_listener
            .local_addr()
            .expect("an address is assigned");
        let silent = tokio::spawn(async move {
            let (_stream, _) = silent_listener.accept().await.expect("a client connects");
            std::future::pending::<()>().await;
        });
        let error = WorldSessionClient::connect(&format!("tcp://{silent_address}"))
            .await
            .expect_err("a silent listener must time out");
        assert!(matches!(
            error,
            WorldSessionWireError::Timeout { ref operation, .. }
                if operation == "request response"
        ));
        silent.abort();

        let handler = Arc::new(TestHandler::new().with_hanging_attach());
        let server = WorldSessionServer::bind(Arc::clone(&handler))
            .await
            .expect("the loopback server binds");
        let client = WorldSessionClient::connect(server.endpoint())
            .await
            .expect("the client verifies bootstrap");
        let error = client
            .attach(ExecutionId::mint(), "tcp/localhost:7447", None)
            .await
            .expect_err("a hung host operation must time out");
        assert!(matches!(
            error,
            WorldSessionWireError::Timeout { ref operation, .. }
                if operation == "host attachment"
        ));
        server.close().await.expect("the server closes cleanly");
    }

    #[tokio::test]
    async fn idle_handshakes_release_the_bounded_connection_permits() {
        let handler = Arc::new(TestHandler::new());
        let server = WorldSessionServer::bind(Arc::clone(&handler))
            .await
            .expect("the loopback server binds");
        let endpoint = parse_endpoint(server.endpoint()).expect("the endpoint parses");
        let mut idle = Vec::with_capacity(MAX_CONNECTIONS);
        for _ in 0..MAX_CONNECTIONS {
            idle.push(
                TcpStream::connect(endpoint)
                    .await
                    .expect("an idle client connects"),
            );
        }
        tokio::time::sleep(HANDSHAKE_TIMEOUT + Duration::from_millis(100)).await;

        WorldSessionClient::connect(server.endpoint())
            .await
            .expect("expired handshakes release permits for a valid client");
        drop(idle);
        server.close().await.expect("the server closes cleanly");
    }
}
