//! Supervisor-owned query and observation endpoints.
//!
//! One endpoint here is unlike the rest: `supervisor/connect` is frozen across
//! every framework line and answers with this supervisor's framework train, which
//! is the whole of what the two binaries negotiate. Every other endpoint below
//! assumes that comparison already agreed, so none of them carries a version.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use phoxal_bundle::RuntimeBundle;
use phoxal_bus::{
    BusHandle, Codec, EndpointDescriptor, IncomingQuery, MessagePack, QueryFailure,
    ServerQueryable, StreamPublisher,
};
use phoxal_model::manifest::ManifestDocument;
use phoxal_protocol::supervisor;
use phoxal_protocol::supervisor::command::{Command, CommandOutcome};
use phoxal_protocol::supervisor::connect::{ConnectReply, ConnectRequest};
use phoxal_protocol::supervisor::execution::SnapshotDocument;
use phoxal_runtime_contract::version::FrameworkVersion;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::state::ExecutionState;

mod logs;
mod telemetry;

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
    let server = declare::<supervisor::endpoint::connect::TopicEndpoint>(&bus).await?;
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
    let server = declare::<supervisor::endpoint::info::TopicEndpoint>(&bus).await?;
    loop {
        let incoming = server.recv().await?;
        let supervisor::info::InfoRequest {} = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(&incoming, &bus, &manifest).await?;
    }
}

async fn serve_snapshots(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let publisher = StreamPublisher::new(bus, &supervisor::topic::owner().snapshot().topic())?;
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
    let server = declare::<supervisor::endpoint::snapshot::CurrentEndpoint>(&bus).await?;
    loop {
        let incoming = server.recv().await?;
        let _: supervisor::snapshot::CurrentRequest = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(&incoming, &bus, &SnapshotDocument::V0(state.snapshot())).await?;
    }
}

/// Read access to the bundle this supervisor is running.
///
/// The supervisor is the only process that knows where the bundle lives, so a
/// client asks it for a path instead of reaching into a filesystem it does not
/// own.
async fn serve_bundle(bus: BusHandle, root: PathBuf) -> Result<()> {
    let server = declare::<supervisor::endpoint::bundle::GetEndpoint>(&bus).await?;
    loop {
        let incoming = server.recv().await?;
        let request: supervisor::bundle::GetRequest = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(&incoming, &bus, &bundle_entry(&root, &request.path)).await?;
    }
}

/// Resolve one requested path against the bundle root.
///
/// An empty path, an absolute path, and any component that is not a plain name
/// are refused outright: they are requests this endpoint never answers, which
/// is a different answer than an entry the bundle does not have.
///
/// Passing that spelling check is not enough to be inside the bundle, because
/// a symlink under the root can point anywhere on the host. Both sides are
/// therefore canonicalized and compared: what leaves this process is a file
/// whose real location is under the bundle's real root, and a path that
/// resolves outside it is refused rather than reported missing, because the
/// entry exists and the supervisor is declining to serve it.
fn bundle_entry(root: &Path, requested: &str) -> supervisor::bundle::GetResponse {
    let path = Path::new(requested);
    let refusable = requested.is_empty()
        || path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if refusable {
        return supervisor::bundle::GetResponse::InvalidPath;
    }
    let Ok(canonical_root) = root.canonicalize() else {
        return supervisor::bundle::GetResponse::Missing;
    };
    let Ok(resolved) = canonical_root.join(path).canonicalize() else {
        return supervisor::bundle::GetResponse::Missing;
    };
    if !resolved.starts_with(&canonical_root) {
        return supervisor::bundle::GetResponse::InvalidPath;
    }
    if !resolved.is_file() {
        return supervisor::bundle::GetResponse::Missing;
    }
    std::fs::read(&resolved).map_or(supervisor::bundle::GetResponse::Missing, |bytes| {
        supervisor::bundle::GetResponse::Found { bytes }
    })
}

/// The two host actions, and nothing about the robot graph.
///
/// The supervisor started no runtime, so it stops none: `phoxal stop` signals
/// the processes the session that launched them recorded, and a client attached
/// to an execution it did not start has nothing here to stop it with.
async fn serve_commands(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let server = declare::<supervisor::endpoint::command::TopicEndpoint>(&bus).await?;
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

async fn declare<E: EndpointDescriptor>(bus: &BusHandle) -> Result<ServerQueryable> {
    Ok(bus.declare_server(E::TOPIC).await?)
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
