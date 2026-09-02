//! Supervisor-owned query and observation endpoints.
//!
//! One endpoint here is unlike the rest: `supervisor/connect` is frozen across
//! every framework line and answers with this supervisor's framework train, which
//! is the whole of what the two binaries negotiate. Every other endpoint below
//! assumes that comparison already agreed, so none of them carries a version.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use crate::bundle::{BundlePath, RuntimeBundle};
use crate::bus::{
    BusHandle, Codec, IncomingQuery, MessagePack, QueryEndpoint, QueryFailure, ServeQuery,
    ServerQueryable, StreamPublisher, Topic,
};
use crate::model::manifest::ManifestDocument;
use crate::supervisor::api as supervisor;
use crate::supervisor::api::command::{Command, CommandOutcome};
use crate::supervisor::api::connect::{ConnectReply, ConnectRequest};
use crate::supervisor::api::execution::SnapshotDocument;
use crate::version::FrameworkVersion;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::state::ExecutionState;

mod logs;
mod telemetry;

/// One reply stays small enough that a malformed or very large asset cannot
/// turn a supervisor query into an unbounded allocation.
const MAX_BUNDLE_CHUNK_BYTES: usize = 64 * 1024;

pub(crate) async fn serve(
    bus: BusHandle,
    state: ExecutionState,
    bundle: RuntimeBundle,
    shutdown: CancellationToken,
) -> Result<()> {
    let mut tasks = JoinSet::new();
    tasks.spawn(serve_connect(bus.clone()));
    tasks.spawn(serve_info(bus.clone(), bundle.manifest().clone()));
    tasks.spawn(serve_snapshots(bus.clone(), state.clone()));
    tasks.spawn(serve_current(bus.clone(), state.clone()));
    tasks.spawn(serve_time_domains(bus.clone(), state.clone()));
    tasks.spawn(serve_current_time_domain(bus.clone(), state.clone()));
    tasks.spawn(serve_bundle(bus.clone(), bundle.root().to_path_buf()));
    tasks.spawn(serve_commands(bus.clone(), state.clone()));
    tasks.spawn(logs::run(bus.clone()));
    tasks.spawn(telemetry::run(bus));

    tokio::select! {
        () = shutdown.cancelled() => {
            tasks.shutdown().await;
            Ok(())
        }
        joined = tasks.join_next() => {
            match joined {
                Some(Ok(Ok(()))) => bail!("a supervisor endpoint task ended before shutdown"),
                Some(Ok(Err(error))) => Err(error),
                Some(Err(error)) => Err(anyhow::anyhow!("a supervisor endpoint task panicked: {error}")),
                None => bail!("all supervisor endpoint tasks ended before shutdown"),
            }
        }
    }
}

/// The frozen attachment bootstrap.
///
/// It answers with this supervisor's framework train and nothing else, and it is
/// declared alongside every other endpoint so a client that disagrees learns
/// that from the first thing it asks rather than from a decode failure. The
/// robot this supervisor runs is not here: a client asks `supervisor/info` for
/// it once the two trains have agreed, which keeps this document exactly what
/// every framework line can decode.
async fn serve_connect(bus: BusHandle) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().connect().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let ConnectRequest::V0 {} = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(&incoming, &bus, &connect_reply()).await?;
    }
}

fn connect_reply() -> ConnectReply {
    ConnectReply::V0 {
        framework: FrameworkVersion::CURRENT,
    }
}

/// Which robot this supervisor is running.
///
/// The answer is the manifest document the supervisor opened, so a client
/// reads exactly what every participant of this execution reads instead of a
/// projection that could disagree with it. The supervisor holds one bundle for
/// the life of the process, so the reply never changes.
async fn serve_info(bus: BusHandle, manifest: ManifestDocument) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().info().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let supervisor::info::InfoRequest {} = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(
            &incoming,
            &bus,
            &supervisor::info::InfoResponse {
                manifest: manifest.clone(),
            },
        )
        .await?;
    }
}

async fn serve_snapshots(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let publisher = StreamPublisher::new(bus, &supervisor::topics().snapshot().owner())?;
    let mut snapshots = state.subscribe();
    publisher.send(SnapshotDocument::V0(snapshots.borrow_and_update().clone()))?;
    loop {
        snapshots
            .changed()
            .await
            .context("the supervisor snapshot authority closed")?;
        publisher.send(SnapshotDocument::V0(snapshots.borrow_and_update().clone()))?;
    }
}

async fn serve_current(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().snapshot().current().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let _: supervisor::snapshot::CurrentRequest = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(&incoming, &bus, &SnapshotDocument::V0(state.snapshot())).await?;
    }
}

/// Publish every complete scheduling-authority replacement in order.
async fn serve_time_domains(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let publisher = StreamPublisher::new(bus, &supervisor::topics().time_domain().owner())?;
    let mut domains = state
        .take_time_domain_updates()
        .context("the supervisor time-domain authority is already being served")?;
    while let Some(domain) = domains.recv().await {
        publisher.send(supervisor::time_domain::TimeDomainStream { domain })?;
    }
    bail!("the supervisor time-domain authority closed")
}

/// Answer the current domain after a client subscribed to its replacement
/// stream, closing the ordinary subscribe/query race.
async fn serve_current_time_domain(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let server = declare(
        &bus,
        &supervisor::topics().time_domain().current().owner(),
    )
    .await?;
    loop {
        let incoming = server.recv().await?;
        let _: supervisor::time_domain::CurrentRequest = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(
            &incoming,
            &bus,
            &supervisor::time_domain::CurrentResponse {
                domain: state.time_domain(),
            },
        )
        .await?;
    }
}

/// Read access to the bundle this supervisor is running.
///
/// The supervisor is the only process that knows where the bundle lives, so a
/// client asks it for a path instead of reaching into a filesystem it does not
/// own.
async fn serve_bundle(bus: BusHandle, root: PathBuf) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().bundle().get().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let request: supervisor::bundle::GetRequest = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        let entry_root = root.clone();
        let response = tokio::task::spawn_blocking(move || bundle_entry(&entry_root, &request))
            .await
            .context("the supervisor bundle reader worker stopped")?;
        reply(&incoming, &bus, &response).await?;
    }
}

/// Resolve one requested path against the bundle root.
///
/// The path has already passed the wire `BundlePath` parser, but a normalized
/// spelling can still escape through a symlink. Both sides are canonicalized,
/// and only regular files under the canonical root are eligible. The static
/// model and staged executables have dedicated contracts, so only immutable
/// assets are exposed through this reader.
fn bundle_entry(
    root: &Path,
    request: &supervisor::bundle::GetRequest,
) -> supervisor::bundle::GetResponse {
    let canonical_root = match root.canonicalize() {
        Ok(root) => root,
        Err(_) => return supervisor::bundle::GetResponse::Refused,
    };
    let candidate = canonical_root.join(request.path.as_str().split('/').collect::<PathBuf>());
    let resolved = match candidate.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            return classify_bundle_candidate_error(&canonical_root, request, &error);
        }
    };
    if !resolved.starts_with(&canonical_root) {
        return supervisor::bundle::GetResponse::InvalidPath;
    }
    if !resolved.is_file() {
        return supervisor::bundle::GetResponse::InvalidPath;
    }
    if !is_served_bundle_asset(&request.path) {
        return supervisor::bundle::GetResponse::Refused;
    }
    read_chunk(&resolved, request.offset)
}

/// Classify failure to resolve the requested entry without treating an existing
/// dangling path as if the bundle did not contain it.
fn classify_bundle_candidate_error(
    root: &Path,
    request: &supervisor::bundle::GetRequest,
    error: &std::io::Error,
) -> supervisor::bundle::GetResponse {
    match error.kind() {
        std::io::ErrorKind::NotFound => match requested_path_status(root, request) {
            Ok(RequestedPathStatus::Invalid) => supervisor::bundle::GetResponse::InvalidPath,
            Ok(RequestedPathStatus::Missing) => supervisor::bundle::GetResponse::Missing,
            Err(_) => supervisor::bundle::GetResponse::Refused,
        },
        std::io::ErrorKind::NotADirectory => supervisor::bundle::GetResponse::InvalidPath,
        _ if is_symlink_loop(error) => supervisor::bundle::GetResponse::InvalidPath,
        _ => supervisor::bundle::GetResponse::Refused,
    }
}

/// How a requested path failed to resolve below an otherwise canonical root.
enum RequestedPathStatus {
    /// A normal component is absent from the bundle.
    Missing,
    /// An existing link cannot produce an admissible path under the bundle root.
    Invalid,
}

/// Inspect every existing component to distinguish absence from a broken or
/// escaping symlink before a later component produces `NotFound`.
fn requested_path_status(
    root: &Path,
    request: &supervisor::bundle::GetRequest,
) -> std::io::Result<RequestedPathStatus> {
    let mut candidate = root.to_path_buf();
    for segment in request.path.as_str().split('/') {
        candidate.push(segment);
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => match candidate.canonicalize() {
                Ok(resolved) if resolved.starts_with(root) => {}
                Ok(_) => return Ok(RequestedPathStatus::Invalid),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound
                            | std::io::ErrorKind::NotADirectory
                    ) || is_symlink_loop(&error) =>
                {
                    return Ok(RequestedPathStatus::Invalid);
                }
                Err(error) => return Err(error),
            },
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RequestedPathStatus::Missing);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(RequestedPathStatus::Invalid)
}

/// `ErrorKind::FilesystemLoop` is still unstable, so Unix loop evidence stays
/// at the portable OS-error boundary until that standard-library variant is
/// available.
fn is_symlink_loop(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ELOOP)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Preserve the distinction between an absent entry, an invalid resolved path,
/// and a bundle entry the supervisor could not serve.
fn classify_bundle_path_error(error: &std::io::Error) -> supervisor::bundle::GetResponse {
    match error.kind() {
        std::io::ErrorKind::NotFound => supervisor::bundle::GetResponse::Missing,
        std::io::ErrorKind::NotADirectory => supervisor::bundle::GetResponse::InvalidPath,
        _ => supervisor::bundle::GetResponse::Refused,
    }
}

fn is_served_bundle_asset(path: &BundlePath) -> bool {
    path.as_str().starts_with("assets/")
}

fn read_chunk(path: &Path, offset: u64) -> supervisor::bundle::GetResponse {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return classify_bundle_path_error(&error),
    };
    let length = match file.metadata().map(|metadata| metadata.len()) {
        Ok(length) => length,
        Err(_) => return supervisor::bundle::GetResponse::Refused,
    };
    if offset >= length {
        return supervisor::bundle::GetResponse::Chunk {
            bytes: Vec::new(),
            eof: true,
        };
    }
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return supervisor::bundle::GetResponse::Refused;
    }
    let remaining = usize::try_from(length.saturating_sub(offset)).unwrap_or(usize::MAX);
    let mut bytes = vec![0; remaining.min(MAX_BUNDLE_CHUNK_BYTES)];
    let read = match file.read(&mut bytes) {
        Ok(read) => read,
        Err(_) => return supervisor::bundle::GetResponse::Refused,
    };
    bytes.truncate(read);
    supervisor::bundle::GetResponse::Chunk {
        eof: read == 0 || offset.saturating_add(read as u64) >= length,
        bytes,
    }
}

/// The two host actions, and nothing about the robot graph.
///
/// The supervisor started no runtime, so it stops none: `phoxal stop` signals
/// the processes the session that launched them recorded, and a client attached
/// to an execution it did not start has nothing here to stop it with.
async fn serve_commands(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().command().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let request: supervisor::command::Request = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        let supervisor::command::Request::V0 { command: request } = request;
        let (outcome, action) = command(&state, request);
        // Acceptance reaches the client before the host is asked to go down;
        // reversing these turns an accepted reboot into an ambiguous
        // no-responder failure at the caller.
        reply(&incoming, &bus, &supervisor::command::Reply::V0 { outcome }).await?;
        action.request().await;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostAction {
    Reboot,
    Poweroff,
}

impl HostAction {
    async fn request(self) {
        let name = match self {
            Self::Reboot => "reboot",
            Self::Poweroff => "power-off",
        };
        let result = tokio::task::spawn_blocking(move || match self {
            Self::Reboot => system_shutdown::reboot(),
            Self::Poweroff => system_shutdown::shutdown(),
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!(action = name, %error, "host action failed"),
            Err(error) => tracing::error!(action = name, %error, "host action task failed"),
        }
    }
}

/// Accept one host request, and say which execution revision it was accepted
/// at.
///
/// The revision is evidence, not a fence: whether cycling this machine's power
/// is safe is the operator's judgment about the machine, and how many times a
/// Ready lease has moved since they last looked says nothing about it.
fn command(state: &ExecutionState, command: Command) -> (CommandOutcome, HostAction) {
    let action = match command {
        Command::Reboot => HostAction::Reboot,
        Command::Poweroff => HostAction::Poweroff,
    };
    (
        CommandOutcome::Accepted {
            at_revision: state.snapshot().revision,
        },
        action,
    )
}

/// Declare the queryable for one supervisor-owned query endpoint.
///
/// The owner-side topic is the only way in, so the key a server binds is the
/// one the api tree rendered for that endpoint and nothing a caller spelled.
async fn declare<E: QueryEndpoint>(
    bus: &BusHandle,
    topic: &Topic<ServeQuery<E>>,
) -> Result<ServerQueryable> {
    Ok(bus.declare_server(topic.key()).await?)
}

async fn decode<T: serde::de::DeserializeOwned>(incoming: &IncomingQuery) -> Result<Option<T>> {
    match MessagePack::decode(&incoming.request_bytes()?) {
        Ok(request) => Ok(Some(request)),
        Err(error) => {
            incoming
                .reply_err(&QueryFailure::invalid_argument(error.to_string()))
                .await?;
            Ok(None)
        }
    }
}

async fn reply<T: serde::Serialize>(
    incoming: &IncomingQuery,
    bus: &BusHandle,
    response: &T,
) -> Result<()> {
    incoming
        .reply(bus, MessagePack::encode(response)?)
        .await
        .map_err(Into::into)
}

#[cfg(test)]
mod endpoint_contract_tests;
