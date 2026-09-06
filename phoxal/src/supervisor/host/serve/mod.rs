//! Supervisor-owned query and observation endpoints.
//!
//! One endpoint here is unlike the rest: `supervisor/connect` is frozen across
//! every framework line and answers with this supervisor's framework train, which
//! is the whole of what the two binaries negotiate. Every other endpoint below
//! assumes that comparison already agreed, so none of them carries a version.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::bundle::{BundlePath, RuntimeBundle};
use crate::bus::{
    BusHandle, Codec, IncomingQuery, LivelinessStatus, MessagePack, QueryEndpoint, QueryFailure,
    ServeQuery, ServerQueryable, StreamPublisher, Topic,
};
use crate::model::manifest::ManifestDocument;
use crate::supervisor::api as supervisor;
use crate::supervisor::api::command::{Command, CommandOutcome};
use crate::supervisor::api::connect::{ConnectReply, ConnectRequest};
use crate::supervisor::api::execution::SnapshotDocument;
use crate::version::FrameworkVersion;
use anyhow::{Context, Result, bail};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::state::ExecutionState;

mod bootstrap;
mod bundle;
mod commands;
mod logs;
mod simulation_attachment;
mod snapshots;
mod telemetry;
mod transport;

use bootstrap::{serve_connect, serve_info};
use bundle::serve_bundle;
use commands::serve_commands;
use simulation_attachment::{
    finish_clean_simulation_removal, serve_attach, serve_attachment_liveness, serve_attachments,
    serve_current_attachment, serve_simulation_end,
};
use snapshots::{
    serve_current, serve_current_time_domain, serve_snapshots, serve_time_domains,
};
use transport::{declare, decode, reply};

#[cfg(test)]
use bootstrap::connect_reply;
#[cfg(test)]
use bundle::{bundle_entry, classify_bundle_path_error};
#[cfg(test)]
use commands::{HostAction, command};

/// One reply stays small enough that a malformed or very large asset cannot
/// turn a supervisor query into an unbounded allocation.
const MAX_BUNDLE_CHUNK_BYTES: usize = 64 * 1024;

/// One intentional supervisor shutdown keeps the execution bus alive long
/// enough for the bound host to remove the native member and for the delegated
/// controller's Ready leases to disappear.
const SIMULATION_REMOVAL_GRACE: Duration = Duration::from_secs(10);

/// Preparing must resolve early enough for the public 5 s attach query to
/// receive an explicit reply rather than timing out while the supervisor can
/// still activate later.
const SIMULATION_PREPARATION_GRACE: Duration = Duration::from_secs(4);

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
    tasks.spawn(serve_attachments(bus.clone(), state.clone()));
    tasks.spawn(serve_current_attachment(bus.clone(), state.clone()));
    tasks.spawn(serve_attach(
        bus.clone(),
        state.clone(),
        shutdown.clone(),
    ));
    tasks.spawn(serve_attachment_liveness(
        bus.clone(),
        state.clone(),
        shutdown.clone(),
    ));
    tasks.spawn(serve_simulation_end(
        bus.clone(),
        state.clone(),
        shutdown.clone(),
    ));
    tasks.spawn(serve_bundle(bus.clone(), bundle.root().to_path_buf()));
    tasks.spawn(serve_commands(bus.clone(), state.clone()));
    tasks.spawn(logs::run(bus.clone()));
    tasks.spawn(telemetry::run(bus.clone()));

    let outcome = tokio::select! {
        () = shutdown.cancelled() => {
            finish_clean_simulation_removal(&bus, &state).await
        }
        joined = tasks.join_next() => {
            match joined {
                Some(Ok(Ok(()))) => bail!("a supervisor endpoint task ended before shutdown"),
                Some(Ok(Err(error))) => Err(error),
                Some(Err(error)) => Err(anyhow::anyhow!("a supervisor endpoint task panicked: {error}")),
                None => bail!("all supervisor endpoint tasks ended before shutdown"),
            }
        }
    };
    tasks.shutdown().await;
    outcome
}

#[cfg(test)]
mod endpoint_contract_tests;
