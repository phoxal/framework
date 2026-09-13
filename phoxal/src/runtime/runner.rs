//! The bounded process runner for the synchronous [`super::Runtime`] contract.
//!
//! The runner owns the process lifecycle around a runtime owner.  It selects
//! one newest-due hardware release, freezes one input cut, executes the step
//! under its host deadline, reserves all output capacity, and only then
//! advances the schedule.  Input and output transport remain explicit host
//! implementations because only a contract owner knows how to encode its
//! generated payloads.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read;
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use zenoh::bytes::Encoding;
use zenoh::key_expr::OwnedKeyExpr;

use super::core::{AcceptedInvocation, Config, OutputAdmission, RegisteredRuntime, RuntimeOwner};
use super::execution_protocol::{self, wire as execution_wire};
use super::input::{
    InputSet, InputSnapshot, OperationCompletionRecord, OperationInputError, ReadError,
    RequestError, TransportInputSet, TransportKeyLookup, TransportValue,
};
use super::operation::{ManagedOperation, OperationOutcome, OperationPolicy};
use super::outputs::{
    OperationWorker, OutputBindings, OutputSet, RuntimeReadRequest, RuntimeWorkSink,
};
use super::schedule::{HardwareInvocation, HardwareSchedule, ScheduleError};
use super::transport::{self, ChangeToken, PreparedOutput, TransportError, WireSample};
use super::{ExecutionTime, RuntimeStatus, StepContext};
use crate::identity::{ExecutionId, ParticipantId};

/// One required output product accepted and published by a runtime boundary.
///
/// The supervisor uses these receipts to distinguish a complete output cut
/// from a runtime that only returned an invocation acknowledgment.
#[derive(Clone, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub struct RuntimeProductReceipt {
    /// Runtime-owned output port identity.
    pub port: String,
    /// Producer sequence carried by the output metadata.
    pub sequence: u64,
    /// Number of records represented by this receipt.
    pub items: u32,
    /// Total encoded body bytes represented by this receipt.
    pub bytes: u64,
}

/// One exact output record emitted by a controlled invocation.
///
/// The supervisor expands ordinary publications over the graph fan-out.  A
/// request already carries its resolved target, while replies are resolved by
/// the originating graph connection at the supervisor.
#[derive(Clone, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub struct RuntimeDeliveryReceipt {
    /// Runtime-owned output port identity.
    pub port: String,
    /// Private Runtime port direction (`publish`, `request`, or `reply`).
    pub direction: String,
    /// Resolved target for a request, when the output carried one.
    pub target: Option<String>,
    /// Producer sequence carried by the output metadata.
    pub sequence: u64,
    /// Zero-based item identity within this port/direction cut.
    pub item: u32,
    /// Exact encoded body byte count.
    pub bytes: u64,
}

/// One input cut receipt returned by a runtime after it froze transport data.
///
/// The source and sequence tie the receipt to the observation publication
/// that the supervisor admitted before issuing the invocation.  This is
/// intentionally separate from an output product receipt because a runtime
/// may also freeze authored graph traffic in the same input cut.
#[derive(Clone, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub struct RuntimeInputReceipt {
    /// Source identity carried by the input publication metadata.
    pub source: String,
    /// Input port identity.
    pub port: String,
    /// Producer sequence carried by the input metadata.
    pub sequence: u64,
    /// Number of frozen records represented by this receipt.
    pub items: u32,
    /// Total encoded body bytes represented by this receipt.
    pub bytes: u64,
}

/// One typed setpoint cut returned by a runtime after acceptance.
///
/// Setpoint outputs are the framework's actuator-facing cut.  The supervisor
/// carries the exact generated payload and expiry to the simulation owner;
/// it never invents a default action when a runtime omits one.
#[derive(Clone, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub struct RuntimeActuation {
    /// Runtime-owned actuator port identity.
    pub port: String,
    /// Encoded generated Protobuf setpoint body.
    pub payload: Vec<u8>,
    /// Simulated time at which this setpoint expires.
    pub valid_until_ns: u64,
}

/// The strict process arguments supplied to one Runtime binary.
///
/// The bundle root, instance identity, and execution identity are explicit so
/// a process can never select a sibling instance or previous execution by
/// package, executable name, or endpoint discovery. The connection is also
/// explicit; no environment fallback or source-tree lookup is permitted.
#[derive(Clone, Debug, Eq, PartialEq, Parser)]
#[command(
    name = "phoxal-runtime",
    about = "Run one admitted Phoxal Runtime instance.",
    long_about = None
)]
pub struct RuntimeLaunch {
    /// Installed immutable bundle directory.
    #[arg(long = "bundle-root", value_name = "PATH")]
    pub bundle_root: PathBuf,
    /// Runtime instance identity selected by the bundle graph.
    #[arg(long = "instance-id", value_name = "ID", value_parser = parse_identifier)]
    pub instance_id: String,
    /// Supervisor-selected execution identity for this process.
    #[arg(long = "execution-id", value_name = "ID", value_parser = parse_execution_id)]
    pub execution_id: ExecutionId,
    /// Supervisor rendezvous endpoint.
    #[arg(
        long = "connect",
        value_name = "ENDPOINT",
        required = true,
        value_parser = parse_endpoint
    )]
    pub connect: String,
}

impl RuntimeLaunch {
    /// Parse the process argv without consulting process environment state.
    pub fn parse() -> crate::Result<Self> {
        Self::try_parse().map_err(anyhow::Error::from)
    }
}

/// The selected executable and configuration entry admitted from a bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeLaunchManifest {
    root: PathBuf,
    robot_id: String,
    instance_id: String,
    executable: PathBuf,
    executable_sha256: String,
    config: Value,
    connections: BTreeMap<String, Vec<String>>,
    artifacts: BTreeMap<String, SourceRuntimeRecord>,
}

impl RuntimeLaunchManifest {
    /// Open and admit one source-side `phoxal/bundle/v0` entry.
    pub fn open(root: impl AsRef<Path>, instance_id: &str) -> crate::Result<Self> {
        let root = root.as_ref().canonicalize().map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: root.as_ref().to_owned(),
                source,
            })
        })?;
        if !root.is_dir() {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: "bundle root is not a directory".to_owned(),
            }));
        }
        let manifest_path = root.join("manifest.json");
        let manifest_metadata = fs::symlink_metadata(&manifest_path).map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: manifest_path.clone(),
                source,
            })
        })?;
        if !manifest_metadata.is_file() || manifest_metadata.file_type().is_symlink() {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: "manifest.json is not a regular file".to_owned(),
            }));
        }
        let bytes = read_bounded(&manifest_path, 16 * 1024 * 1024)?;
        let manifest =
            serde_json::from_slice::<SourceBundleManifest>(&bytes).map_err(|source| {
                anyhow::anyhow!(RunnerError::BundleJson {
                    path: manifest_path.clone(),
                    source,
                })
            })?;
        if manifest.schema != "phoxal/bundle/v0" {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!(
                    "unsupported bundle schema `{}`; expected `phoxal/bundle/v0`",
                    manifest.schema
                ),
            }));
        }
        parse_identifier(instance_id)
            .map_err(|message| anyhow::anyhow!(RunnerError::BundleInvalid { message }))?;
        let executable = manifest
            .executables
            .iter()
            .find(|entry| entry.instance == instance_id)
            .ok_or_else(|| {
                anyhow::anyhow!(RunnerError::UnknownInstance {
                    instance: instance_id.to_owned(),
                })
            })?;
        let executable_sha256 = executable.sha256.clone();
        let relative = safe_relative_path(&executable.path)?;
        let executable_path = root.join(relative);
        let path_metadata = fs::symlink_metadata(&executable_path).map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: executable_path.clone(),
                source,
            })
        })?;
        if !path_metadata.is_file() || path_metadata.file_type().is_symlink() {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!("executable for `{instance_id}` is not a regular file"),
            }));
        }
        let canonical_executable = executable_path.canonicalize().map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: executable_path.clone(),
                source,
            })
        })?;
        if !canonical_executable.starts_with(&root) {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!("executable for `{instance_id}` resolves outside the bundle root"),
            }));
        }
        let metadata = fs::symlink_metadata(&canonical_executable).map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: canonical_executable.clone(),
                source,
            })
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!("executable for `{instance_id}` is not a regular file"),
            }));
        }
        verify_executable(&canonical_executable, executable)?;

        let config = if instance_id == "brain" {
            Value::Object(serde_json::Map::new())
        } else {
            manifest
                .document
                .services
                .get(instance_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(RunnerError::BundleInvalid {
                        message: format!(
                            "executable `{instance_id}` has no matching services entry"
                        ),
                    })
                })?
                .config
                .clone()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
        };
        if config.is_null() {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!("configuration for `{instance_id}` is explicit null"),
            }));
        }
        let connections = manifest
            .document
            .connections
            .iter()
            .map(|(consumer, sources)| (consumer.clone(), sources.as_slice().to_vec()))
            .collect();
        let artifacts = manifest
            .executables
            .iter()
            .filter_map(|entry| {
                entry
                    .artifact
                    .as_ref()
                    .map(|artifact| (entry.instance.clone(), artifact.runtime.clone()))
            })
            .collect();
        Ok(Self {
            root,
            robot_id: manifest.robot_id,
            instance_id: instance_id.to_owned(),
            executable: canonical_executable,
            executable_sha256,
            config,
            connections,
            artifacts,
        })
    }

    /// Installed bundle root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Compiled robot identity.
    #[must_use]
    pub fn robot_id(&self) -> &str {
        &self.robot_id
    }

    /// Admitted runtime instance identity.
    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Verified executable path for the selected instance.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The verified SHA-256 digest recorded for the selected executable.
    #[must_use]
    pub fn executable_sha256(&self) -> &str {
        &self.executable_sha256
    }

    /// Owned authored configuration value for typed decoding.
    #[must_use]
    pub fn config(&self) -> &Value {
        &self.config
    }

    /// Resolve the canonical caller ordinal for every Commands port in this
    /// target's authored graph.  The ordinal is an exact lexical position of
    /// `{caller-instance}.{request-field}`, not a hash, so a carried rank is
    /// independently checkable by the receiving runtime.
    fn command_ranks(
        &self,
        target_instance: &str,
    ) -> crate::Result<BTreeMap<(String, String), u64>> {
        let mut callers: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
        for (consumer, sources) in &self.connections {
            let (caller_instance, request_field) =
                parse_graph_endpoint(consumer).map_err(|error| {
                    anyhow::anyhow!(RunnerError::BundleInvalid {
                        message: format!("connection consumer `{consumer}` is invalid: {error}"),
                    })
                })?;
            for source in sources {
                let (source_instance, source_port) =
                    parse_graph_endpoint(source).map_err(|error| {
                        anyhow::anyhow!(RunnerError::BundleInvalid {
                            message: format!("connection source `{source}` is invalid: {error}"),
                        })
                    })?;
                if source_instance == target_instance {
                    callers
                        .entry(source_port)
                        .or_default()
                        .insert((caller_instance.clone(), request_field.clone()));
                }
            }
        }
        let mut ranks = BTreeMap::new();
        for (port, callers) in callers {
            for (rank, (caller_instance, request_field)) in callers.into_iter().enumerate() {
                ranks.insert(
                    (port.clone(), format!("{caller_instance}.{request_field}")),
                    rank as u64,
                );
            }
        }
        Ok(ranks)
    }

    fn caller_rank_for(
        &self,
        target_instance: &str,
        target_port: &str,
        caller_identity: &str,
    ) -> crate::Result<u64> {
        self.command_ranks(target_instance)?
            .remove(&(target_port.to_owned(), caller_identity.to_owned()))
            .ok_or_else(|| {
                anyhow::anyhow!(RunnerError::BundleInvalid {
                    message: format!(
                        "caller `{caller_identity}` is not connected to `{target_instance}.{target_port}`"
                    ),
                })
            })
    }

    /// Resolve one generated input field through the immutable graph and
    /// artifact records retained in this bundle.
    fn input_routes(
        &self,
        field: &super::transport::InputTransportField,
    ) -> crate::Result<Vec<ResolvedInputRoute>> {
        let Some(signature) = field.signature else {
            if field.kind == super::input::InputKind::Operation {
                return Ok(Vec::new());
            }
            let consumer = format!("{}.{}", self.instance_id, field.name);
            let sources = self.connections.get(&consumer).ok_or_else(|| {
                anyhow::anyhow!(RunnerError::BundleInvalid {
                    message: format!(
                        "input `{consumer}` has no authored connection in the source bundle"
                    ),
                })
            })?;
            if sources.is_empty() {
                return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                    message: format!("input `{consumer}` has an empty authored connection"),
                }));
            }
            let direction = InputDirection::for_kind(field.kind)?;
            let expected_kind = direction.expected_kind(field.kind);
            let mut routes = Vec::with_capacity(sources.len());
            let mut first_binding = None;
            for source in sources {
                let (source_instance, source_port) =
                    parse_graph_endpoint(source).map_err(|message| {
                        anyhow::anyhow!(RunnerError::BundleInvalid {
                            message: format!(
                                "connection `{consumer} <- {source}` is invalid: {message}"
                            ),
                        })
                    })?;
                let runtime = self.artifacts.get(&source_instance).ok_or_else(|| {
                    anyhow::anyhow!(RunnerError::BundleInvalid {
                        message: format!(
                            "connection `{consumer} <- {source}` has no admitted producer artifact"
                        ),
                    })
                })?;
                let (binding, source_max_bytes, source_max_items, source_request_max_bytes) =
                    if expected_kind == crate::port::PortKind::Commands
                        && direction == InputDirection::Reply
                    {
                        let input = runtime
                        .inputs
                        .iter()
                        .find(|candidate| candidate.port.as_deref() == Some(source_port.as_str()))
                        .ok_or_else(|| {
                            anyhow::anyhow!(RunnerError::BundleInvalid {
                                message: format!(
                                    "connection `{consumer} <- {source}` has no Commands input port"
                                ),
                            })
                        })?;
                        let binding = input.signature.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(RunnerError::BundleInvalid {
                            message: format!(
                                "connection `{consumer} <- {source}` Commands input has no signature"
                            ),
                        })
                    })?;
                        let reply = runtime
                        .service_outputs
                        .iter()
                        .find(|candidate| {
                            candidate.kind == "reply"
                                && candidate.input.as_deref() == Some(input.name.as_str())
                        })
                        .ok_or_else(|| {
                            anyhow::anyhow!(RunnerError::BundleInvalid {
                                message: format!(
                                    "connection `{consumer} <- {source}` Commands target has no reply binding"
                                ),
                            })
                        })?;
                        (
                            SourcePortSignature::to_binding(binding)?,
                            reply.max_bytes,
                            reply.max_items,
                            input.max_bytes,
                        )
                    } else {
                        let output = runtime
                        .service_outputs
                        .iter()
                        .chain(runtime.transient_outputs.iter())
                        .find(|candidate| candidate.port.as_deref() == Some(source_port.as_str()))
                        .ok_or_else(|| {
                            anyhow::anyhow!(RunnerError::BundleInvalid {
                                message: format!(
                                    "connection `{consumer} <- {source}` has no served output port"
                                ),
                            })
                        })?;
                        let binding = output.signature.as_ref().ok_or_else(|| {
                            anyhow::anyhow!(RunnerError::BundleInvalid {
                                message: format!(
                                    "connection `{consumer} <- {source}` output has no signature"
                                ),
                            })
                        })?;
                        (
                            SourcePortSignature::to_binding(binding)?,
                            output.max_bytes,
                            output.max_items,
                            output.max_request_bytes,
                        )
                    };
                if binding.kind != expected_kind {
                    return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                        message: format!(
                            "connection `{consumer} <- {source}` expects `{}` but source provides `{}`",
                            expected_kind.as_str(),
                            binding.kind.as_str()
                        ),
                    }));
                }
                if let Some(previous) = &first_binding
                    && previous != &binding
                {
                    return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                        message: format!(
                            "connection `{consumer}` has source signatures that do not match"
                        ),
                    }));
                }
                first_binding = Some(binding.clone());
                let max_bytes = field.max_bytes.or(source_max_bytes).ok_or_else(|| {
                    anyhow::anyhow!(RunnerError::BundleInvalid {
                        message: format!(
                            "connection `{consumer} <- {source}` has no bounded encoded-byte limit"
                        ),
                    })
                })?;
                let max_items = field.max_items.or(source_max_items).unwrap_or(1);
                let request_max_bytes = if direction == InputDirection::Reply {
                    Some(source_request_max_bytes.ok_or_else(|| {
                        anyhow::anyhow!(RunnerError::BundleInvalid {
                            message: format!(
                                "connection `{consumer} <- {source}` has no bounded request-byte limit"
                            ),
                        })
                    })?)
                } else {
                    None
                };
                if max_items == 0 || max_bytes == 0 {
                    return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                        message: format!(
                            "connection `{consumer} <- {source}` has a zero transport bound"
                        ),
                    }));
                }
                let (caller_identity, caller_rank) = if direction == InputDirection::Reply {
                    let caller_identity = format!("{}.{}", self.instance_id, field.name);
                    let caller_rank =
                        self.caller_rank_for(&source_instance, &source_port, &caller_identity)?;
                    (Some(caller_identity), Some(caller_rank))
                } else {
                    (None, None)
                };
                routes.push(ResolvedInputRoute {
                    field: field.name,
                    binding,
                    source_instance,
                    source_port,
                    direction,
                    max_items,
                    max_bytes,
                    request_max_bytes,
                    caller_identity,
                    caller_rank,
                });
            }
            return Ok(routes);
        };
        if field.kind != super::input::InputKind::Commands {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!(
                    "input `{}` carries an explicit descriptor but is not a Commands listener",
                    field.name
                ),
            }));
        }
        let max_items = field.max_items.unwrap_or(1);
        let max_bytes = field.max_bytes.ok_or_else(|| {
            anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!("Commands input `{}` has no byte bound", field.name),
            })
        })?;
        if max_items == 0 || max_bytes == 0 {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!("Commands input `{}` has a zero transport bound", field.name),
            }));
        }
        Ok(vec![ResolvedInputRoute {
            field: field.name,
            binding: super::transport::PortBinding::from_signature(signature),
            source_instance: self.instance_id.clone(),
            source_port: signature.name.to_owned(),
            direction: InputDirection::Request,
            max_items,
            max_bytes,
            request_max_bytes: Some(max_bytes),
            caller_identity: None,
            caller_rank: None,
        }])
    }

    /// Decode the selected configuration into the exact runtime type.
    pub fn decode_config<R: RegisteredRuntime>(&self) -> crate::Result<R::Config> {
        decode_config(self.config.clone())
    }
}

/// A source of one immutable input cut at the selected hardware boundary.
pub trait InputSource<R: RegisteredRuntime> {
    /// Freeze and return the complete input snapshot for one candidate.
    fn freeze(&mut self, candidate: &HardwareInvocation) -> crate::Result<R::Inputs>;

    /// Take receipts for the transport records frozen by the last candidate.
    ///
    /// Direct in-process inputs have no distributed publication to prove and
    /// therefore return no receipts by default.
    fn take_input_receipts(&mut self) -> Vec<RuntimeInputReceipt> {
        Vec::new()
    }

    /// Update the receiver queue's active controlled timeline.  The queue
    /// fence is changed before a reset clears retained samples, so an old
    /// sample cannot cross into a fresh timeline while a worker is unwinding.
    fn set_timeline(&mut self, _timeline_id: &str) -> crate::Result<()> {
        Ok(())
    }

    /// Stop subscriptions, pending requests, and managed operation workers.
    fn stop(&mut self) -> crate::Result<()> {
        Ok(())
    }

    /// Clear pending input work before a fresh execution initialization.
    fn reset(&mut self) -> crate::Result<()> {
        Ok(())
    }
}

/// A sink that reserves and publishes one complete accepted output batch.
pub trait OutputSink<R>: OutputAdmission<R::Outputs>
where
    R: RegisteredRuntime,
{
    /// Prepare invocation-scoped output data before state acceptance.
    fn prepare(&mut self, _context: &super::StepContext) -> crate::Result<()> {
        Ok(())
    }

    /// Prepare state and setpoint projections from the candidate next state.
    fn prepare_state(
        &mut self,
        _service: &R,
        _state: &R::State,
        _context: &super::StepContext,
    ) -> crate::Result<()> {
        Ok(())
    }

    /// Publish initialized state projections before the first invocation.
    fn bootstrap(
        &mut self,
        _service: &R,
        _state: &R::State,
        _now: ExecutionTime,
    ) -> crate::Result<()> {
        Ok(())
    }

    /// Admit transport completions and requests without blocking the compute
    /// owner.  The default keeps direct in-process sinks transport-free.
    fn poll(&mut self) -> crate::Result<()> {
        Ok(())
    }

    /// Publish an already-reserved complete invocation.
    fn publish(
        &mut self,
        accepted: AcceptedInvocation<R::Outputs, Self::Reservation>,
    ) -> crate::Result<()>;

    /// Stamp records from a controlled invocation before they are published.
    /// Direct sinks remain transport-free through the default implementation.
    fn prepare_delivery(&mut self, _boundary: u64, _timeline_id: &str) -> crate::Result<()> {
        Ok(())
    }

    /// Take the receipts produced by the most recently published batch.
    ///
    /// A transport adapter may aggregate several records for one output port.
    /// The default keeps direct in-process sinks free of transport concerns.
    fn take_product_receipts(&mut self) -> Vec<RuntimeProductReceipt> {
        Vec::new()
    }

    /// Take exact graph records emitted by the most recently published
    /// controlled invocation.
    fn take_delivery_receipts(&mut self) -> Vec<RuntimeDeliveryReceipt> {
        Vec::new()
    }

    /// Take the accepted actuator-facing setpoint cut from the last publish.
    fn take_actuations(&mut self) -> Vec<RuntimeActuation> {
        Vec::new()
    }

    /// Stop publishers and wait for managed operation cleanup.
    fn stop(&mut self) -> crate::Result<()> {
        Ok(())
    }

    /// Clear pending output work before a fresh execution initialization.
    fn reset(&mut self) -> crate::Result<()> {
        Ok(())
    }
}

/// A host-monotonic clock used by [`RuntimeRunner::run_until_stop`].
pub trait RuntimeClock {
    /// Return elapsed host time from this clock's origin.
    fn now(&mut self) -> ExecutionTime;

    /// Wait until a logical release, returning early when the host is stopped.
    fn wait_until(&mut self, release: ExecutionTime) -> crate::Result<()>;
}

/// A system host clock with one monotonic origin.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// Start a clock at the current host-monotonic instant.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeClock for SystemClock {
    fn now(&mut self) -> ExecutionTime {
        ExecutionTime::from(self.origin.elapsed())
    }

    fn wait_until(&mut self, release: ExecutionTime) -> crate::Result<()> {
        let target = Duration::from(release);
        if let Some(remaining) = target.checked_sub(self.origin.elapsed()) {
            std::thread::sleep(remaining);
        }
        Ok(())
    }
}

/// Run one compiled runtime process against its supervisor-owned execution.
///
/// The process owns exactly one bus session and one serialized runner.  The
/// session is opened only for the execution id resolved from the explicit
/// rendezvous endpoint, and the Ready lease is held for the whole time the
/// runner is live.  Generated Commands input fields and transient Protobuf
/// output fields use their descriptor-owned codecs.  Unsupported fields fail
/// explicitly rather than becoming invented defaults or silent drops.
pub(crate) fn run_transport<R>(service: R) -> crate::Result<()>
where
    R: RegisteredRuntime,
    R::Inputs: TransportInputSet + super::input::TransportInputSink,
{
    R::__retain_artifact_metadata();
    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    enforce_process_boundary(tokio_runtime.block_on(run_transport_async(service)))
}

async fn run_transport_async<R>(service: R) -> crate::Result<()>
where
    R: RegisteredRuntime,
    R::Inputs: TransportInputSet + super::input::TransportInputSink,
{
    let launch = RuntimeLaunch::parse()?;
    let manifest = RuntimeLaunchManifest::open(&launch.bundle_root, &launch.instance_id)?;
    let config = manifest.decode_config::<R>()?;
    let participant = ParticipantId::new(launch.instance_id.clone())
        .map_err(|error| anyhow::anyhow!("invalid runtime participant id: {error}"))?;
    let shutdown = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "runtime could not install its Ctrl-C listener");
        }
    };
    tokio::pin!(shutdown);
    let execution = launch.execution_id;

    let correlations = Arc::new(Mutex::new(BTreeMap::new()));
    let expired_correlations = Arc::new(Mutex::new(BTreeSet::new()));
    let operation_completions = Arc::new(Mutex::new(Vec::new()));
    let exchange_completions = Arc::new(Mutex::new(Vec::new()));
    let input = ExecutionInputAdapter::<R>::unbound().with_shared_state(
        Arc::clone(&correlations),
        Arc::clone(&expired_correlations),
        Arc::clone(&operation_completions),
        Arc::clone(&exchange_completions),
    );
    let output = ExecutionOutputAdapter::<R>::unbound().with_shared_state(
        correlations,
        expired_correlations,
        operation_completions,
        exchange_completions,
    );

    // Runtime initialization is local and serialized before transport Ready
    // is declared.  The adapters retain the execution-scoped handle and
    // therefore cannot accidentally read or publish another execution root.
    let (owner, bus) = tokio::select! {
        biased;
        _ = &mut shutdown => return Ok(()),
        result = crate::bus::BusOwner::open(crate::bus::BusConfig::for_participant(
            execution,
            participant,
            vec![launch.connect.clone()],
        )) => result?,
    };
    let mut input = input;
    let mut output = output;
    if let Err(error) = input.bind(bus.clone(), &manifest).await {
        let _ = owner.close().await;
        return Err(error);
    }
    if let Err(error) = output
        .bind(bus.clone(), &launch.instance_id, &manifest)
        .await
    {
        let _ = owner.close().await;
        return Err(error);
    }

    let mut runner =
        match RuntimeRunner::new(service, ExecutionTime::default(), config, input, output) {
            Ok(runner) => runner,
            Err(error) => {
                let _ = owner.close().await;
                return Err(error);
            }
        };

    let admit_subscriber = declare_execution_subscriber(&bus, &launch.instance_id, "admit").await?;
    let invoke_subscriber =
        declare_execution_subscriber(&bus, &launch.instance_id, "invoke").await?;
    let reset_subscriber = declare_execution_subscriber(&bus, &launch.instance_id, "reset").await?;
    let admission = tokio::select! {
        biased;
        _ = &mut shutdown => {
            let _ = owner.close().await;
            return Ok(());
        }
        result = recv_execution::<execution_wire::AdmitExecutionRequest>(&admit_subscriber) => result?,
    };
    let execution_mode = match execution_wire::ExecutionMode::try_from(admission.mode) {
        Ok(execution_wire::ExecutionMode::Unspecified) => {
            let response = execution_wire::AdmitExecutionResponse {
                admitted: false,
                unsupported_contracts: Vec::new(),
                detail: Some("execution scheduling mode is required".to_owned()),
            };
            let _ = publish_execution(&bus, &launch.instance_id, "admit-response", &response).await;
            let _ = owner.close().await;
            return Err(anyhow::anyhow!("execution scheduling mode is required"));
        }
        Ok(mode) => mode,
        Err(_) => {
            let response = execution_wire::AdmitExecutionResponse {
                admitted: false,
                unsupported_contracts: Vec::new(),
                detail: Some("unknown execution scheduling mode".to_owned()),
            };
            let _ = publish_execution(&bus, &launch.instance_id, "admit-response", &response).await;
            let _ = owner.close().await;
            return Err(anyhow::anyhow!("unknown execution scheduling mode"));
        }
    };
    if let Err((detail, unsupported_contracts)) = validate_execution_admission::<R>(
        &manifest,
        &admission,
        execution_mode,
        &execution.to_string(),
    ) {
        let response = execution_wire::AdmitExecutionResponse {
            admitted: false,
            unsupported_contracts,
            detail: Some(detail.clone()),
        };
        let _ = publish_execution(&bus, &launch.instance_id, "admit-response", &response).await;
        let _ = owner.close().await;
        return Err(anyhow::anyhow!("runtime admission refused: {detail}"));
    }
    let response = execution_wire::AdmitExecutionResponse {
        admitted: true,
        unsupported_contracts: Vec::new(),
        detail: None,
    };
    publish_execution(&bus, &launch.instance_id, "admit-response", &response).await?;

    publish_execution(
        &bus,
        &launch.instance_id,
        "ready",
        &execution_wire::Ready {
            execution_id: execution.to_string(),
            timeline_id: admission.timeline_id.clone(),
            runtime_instance: launch.instance_id.clone(),
        },
    )
    .await?;

    let result = match execution_mode {
        execution_wire::ExecutionMode::Unspecified => {
            Err(anyhow::anyhow!("execution scheduling mode is required"))
        }
        execution_wire::ExecutionMode::Hardware => {
            let mut ticker = tokio::time::interval(R::SPEC.period.as_duration());
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut clock = SystemClock::new();
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown => break Ok(()),
                    _ = ticker.tick() => match runner.poll(clock.now()) {
                        Ok(PollOutcome::NotDue { .. } | PollOutcome::Accepted { .. }) => {}
                        Ok(PollOutcome::Stopped) => break Ok(()),
                        Err(error) => break Err(error),
                    },
                }
            }
        }
        execution_wire::ExecutionMode::Controlled => {
            run_controlled_transport(
                &mut runner,
                shutdown,
                ControlledTransport {
                    manifest: &manifest,
                    launch: &launch,
                    bus: &bus,
                    invoke_subscriber,
                    reset_subscriber,
                    timeline_id: admission.timeline_id,
                    quantum_ns: admission.quantum_ns,
                },
            )
            .await
        }
    };

    let stop_result = runner.stop();
    let _close_report = owner.close().await;
    result.and(stop_result)
}

type ExecutionSubscriber =
    zenoh::pubsub::Subscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>;

const EXECUTION_CHANNEL_CAPACITY: usize = 64;
const MAX_RETAINED_CONTROLLED_INVOCATIONS: usize = 256;

async fn declare_execution_subscriber(
    bus: &crate::bus::BusHandle,
    instance: &str,
    leg: &str,
) -> crate::Result<ExecutionSubscriber> {
    let key = execution_protocol::key(bus, instance, leg);
    let key = OwnedKeyExpr::new(key).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let session = bus.session()?;
    session
        .declare_subscriber(key)
        .with(zenoh::handlers::FifoChannel::new(
            EXECUTION_CHANNEL_CAPACITY,
        ))
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

async fn recv_execution<M>(subscriber: &ExecutionSubscriber) -> crate::Result<M>
where
    M: prost::Message + Default,
{
    let sample = subscriber
        .recv_async()
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    if !execution_protocol::has_encoding(&sample) {
        return Err(anyhow::anyhow!(
            "private execution message used an unexpected encoding"
        ));
    }
    execution_protocol::decode(sample.payload().to_bytes().as_ref())
}

async fn publish_execution<M: prost::Message>(
    bus: &crate::bus::BusHandle,
    instance: &str,
    leg: &str,
    message: &M,
) -> crate::Result<()> {
    let key = execution_protocol::key(bus, instance, leg);
    let payload = execution_protocol::encode(message)?;
    let session = bus.session()?;
    session
        .put(key, payload)
        .encoding(Encoding::from(
            execution_protocol::PROTOBUF_ENCODING.to_owned(),
        ))
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(())
}

fn validate_execution_admission<R: RegisteredRuntime>(
    manifest: &RuntimeLaunchManifest,
    request: &execution_wire::AdmitExecutionRequest,
    mode: execution_wire::ExecutionMode,
    expected_execution: &str,
) -> Result<(), (String, Vec<String>)> {
    let reject = |detail: String, unsupported: Vec<String>| Err((detail, unsupported));
    if request.execution_id != expected_execution || request.timeline_id.is_empty() {
        return reject(
            "execution admission requires execution and timeline identities".to_owned(),
            Vec::new(),
        );
    }
    let Some(expected_digest) = decode_digest(manifest.executable_sha256()) else {
        return reject(
            "runtime executable digest is not a 32-byte hexadecimal value".to_owned(),
            Vec::new(),
        );
    };
    if request.artifact_digest != expected_digest {
        return reject(
            "runtime executable digest does not match the admitted artifact".to_owned(),
            Vec::new(),
        );
    }
    let mut unsupported_contracts = Vec::new();
    let exact_capabilities = BTreeSet::from(["invocation", "reset", "delivery-ack"]);
    if request.required_contracts.len() != 1 {
        unsupported_contracts.extend(
            request
                .required_contracts
                .iter()
                .map(|contract| contract.protocol.clone()),
        );
        return reject(
            "execution admission requires exactly one supported contract requirement".to_owned(),
            unsupported_contracts,
        );
    }
    let requirement = &request.required_contracts[0];
    let capabilities = requirement
        .capabilities
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if requirement.protocol != "phoxal.execution.v1" || capabilities != exact_capabilities {
        unsupported_contracts.push(requirement.protocol.clone());
        unsupported_contracts.extend(
            capabilities
                .difference(&exact_capabilities)
                .map(|capability| format!("phoxal.execution.v1/{capability}")),
        );
        return reject(
            "execution admission requires the exact invocation, reset, and delivery-ack capabilities".to_owned(),
            unsupported_contracts,
        );
    }
    match mode {
        execution_wire::ExecutionMode::Unspecified => {
            return reject(
                "execution scheduling mode is required".to_owned(),
                Vec::new(),
            );
        }
        execution_wire::ExecutionMode::Hardware => {
            if request.quantum_ns != 0 {
                return reject(
                    "hardware execution must not carry a simulation quantum".to_owned(),
                    Vec::new(),
                );
            }
        }
        execution_wire::ExecutionMode::Controlled => {
            if request.quantum_ns == 0 {
                return reject(
                    "controlled execution requires a positive quantum".to_owned(),
                    Vec::new(),
                );
            }
            if !R::SPEC.period.as_nanos().is_multiple_of(request.quantum_ns) {
                return reject(
                    format!(
                        "runtime period {} ns is not an exact multiple of controlled quantum {} ns",
                        R::SPEC.period.as_nanos(),
                        request.quantum_ns
                    ),
                    Vec::new(),
                );
            }
        }
    }
    Ok(())
}

struct ControlledTransport<'a> {
    manifest: &'a RuntimeLaunchManifest,
    launch: &'a RuntimeLaunch,
    bus: &'a crate::bus::BusHandle,
    invoke_subscriber: ExecutionSubscriber,
    reset_subscriber: ExecutionSubscriber,
    timeline_id: String,
    quantum_ns: u64,
}

async fn run_controlled_transport<R, Inputs, Outputs>(
    runner: &mut RuntimeRunner<R, Inputs, Outputs>,
    mut shutdown: std::pin::Pin<&mut impl std::future::Future<Output = ()>>,
    transport: ControlledTransport<'_>,
) -> crate::Result<()>
where
    R: RegisteredRuntime,
    R::Inputs: InputSnapshot,
    Inputs: InputSource<R>,
    Outputs: OutputSink<R>,
{
    let ControlledTransport {
        manifest,
        launch,
        bus,
        invoke_subscriber,
        reset_subscriber,
        mut timeline_id,
        quantum_ns,
    } = transport;
    let mut last_boundary = None;
    let mut accepted = BTreeMap::<u64, execution_wire::InvocationAccepted>::new();
    loop {
        tokio::select! {
            biased;
            _ = shutdown.as_mut() => break Ok(()),
            reset = reset_subscriber.recv_async() => {
                let sample = reset.map_err(|error| anyhow::anyhow!(error.to_string()))?;
                let request = decode_execution_sample::<execution_wire::ResetExecutionRequest>(&sample)?;
                let valid = request.execution_id == bus.execution().to_string()
                    && request.retired_timeline_id == timeline_id
                    && request.next_timeline_id != timeline_id
                    && !request.next_timeline_id.is_empty()
                    && last_boundary.is_none_or(|boundary| request.completed_boundary >= boundary);
                if !valid {
                    let response = execution_wire::ResetExecutionResponse {
                        accepted: false,
                        detail: Some("reset execution, timeline, or completed boundary is stale".to_owned()),
                    };
                    publish_execution(bus, &launch.instance_id, "reset-response", &response).await?;
                    continue;
                }
                let config = manifest.decode_config::<R>()?;
                match runner
                    .set_controlled_timeline(&request.next_timeline_id)
                    .and_then(|()| runner.reset(ExecutionTime::default(), config))
                {
                    Ok(()) => {
                        timeline_id = request.next_timeline_id;
                        last_boundary = None;
                        accepted.clear();
                        let response = execution_wire::ResetExecutionResponse {
                            accepted: true,
                            detail: None,
                        };
                        publish_execution(bus, &launch.instance_id, "reset-response", &response).await?;
                    }
                    Err(error) => {
                        let detail = format!("runtime reset failed: {error:#}");
                        let response = execution_wire::ResetExecutionResponse {
                            accepted: false,
                            detail: Some(detail.clone()),
                        };
                        publish_execution(bus, &launch.instance_id, "reset-response", &response).await?;
                        let failure = execution_wire::RuntimeFailure {
                            execution_id: bus.execution().to_string(),
                            timeline_id: timeline_id.clone(),
                            runtime_instance: launch.instance_id.clone(),
                            boundary: request.completed_boundary,
                            reason: detail,
                        };
                        publish_execution(bus, &launch.instance_id, "failure", &failure).await?;
                        break Err(error);
                    }
                }
            }
            invocation = invoke_subscriber.recv_async() => {
                let sample = invocation.map_err(|error| anyhow::anyhow!(error.to_string()))?;
                let request = decode_execution_sample::<execution_wire::Invocation>(&sample)?;
                if request.execution_id != bus.execution().to_string()
                    || request.runtime_instance != launch.instance_id
                    || request.timeline_id != timeline_id
                {
                    send_runtime_failure(
                        bus,
                        launch,
                        &timeline_id,
                        request.boundary,
                        "invocation execution, timeline, or runtime identity is stale",
                    ).await?;
                    continue;
                }
                let expected_logical_time = request
                    .boundary
                    .checked_mul(quantum_ns)
                    .ok_or_else(|| anyhow::anyhow!("controlled invocation time overflow"))?;
                if request.logical_time_ns != expected_logical_time {
                    send_runtime_failure(
                        bus,
                        launch,
                        &timeline_id,
                        request.boundary,
                        "invocation logical time does not match its boundary and quantum",
                    )
                    .await?;
                    continue;
                }
                if let Some(previous) = last_boundary {
                    if request.boundary < previous {
                        send_runtime_failure(
                            bus,
                            launch,
                            &timeline_id,
                            request.boundary,
                            "late invocation was rejected after a newer boundary completed",
                        ).await?;
                        continue;
                    }
                    if request.boundary == previous {
                        if let Some(response) = accepted.get(&request.boundary) {
                            publish_execution(bus, &launch.instance_id, "accepted", response).await?;
                        } else {
                            send_runtime_failure(
                                bus,
                                launch,
                                &timeline_id,
                                request.boundary,
                                "duplicate invocation has no retained acceptance receipt",
                            ).await?;
                        }
                        continue;
                    }
                    let expected = next_due_boundary(previous, R::SPEC.period.as_nanos(), quantum_ns)
                        .ok_or_else(|| anyhow::anyhow!("controlled invocation boundary overflow"))?;
                    if request.boundary != expected {
                        send_runtime_failure(
                            bus,
                            launch,
                            &timeline_id,
                            request.boundary,
                            "invocation skipped a required due boundary",
                        )
                        .await?;
                        continue;
                    }
                } else if request.boundary != 0 {
                    send_runtime_failure(
                        bus,
                        launch,
                        &timeline_id,
                        request.boundary,
                        "the first controlled invocation must be boundary zero",
                    )
                    .await?;
                    continue;
                }
                let now = ExecutionTime::from_nanos(request.logical_time_ns);
                let outcome = match runner.invoke_controlled(request.boundary, now, &timeline_id) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let detail = format!("controlled invocation failed: {error:#}");
                        let failure = execution_wire::RuntimeFailure {
                            execution_id: bus.execution().to_string(),
                            timeline_id: timeline_id.clone(),
                            runtime_instance: launch.instance_id.clone(),
                            boundary: request.boundary,
                            reason: detail,
                        };
                        publish_execution(bus, &launch.instance_id, "failure", &failure).await?;
                        break Err(error);
                    }
                };
                let response = execution_wire::InvocationAccepted {
                    execution_id: request.execution_id,
                    timeline_id: request.timeline_id,
                    runtime_instance: request.runtime_instance,
                    boundary: outcome.boundary,
                    required_products: outcome
                        .required_products
                        .into_iter()
                        .map(|receipt| execution_wire::ProductReceipt {
                            port: receipt.port,
                            sequence: receipt.sequence,
                            items: receipt.items,
                            bytes: receipt.bytes,
                        })
                        .collect(),
                    required_inputs: outcome
                        .required_inputs
                        .into_iter()
                        .map(|receipt| execution_wire::InputReceipt {
                            source: receipt.source,
                            port: receipt.port,
                            sequence: receipt.sequence,
                            items: receipt.items,
                            bytes: receipt.bytes,
                        })
                        .collect(),
                    actuations: outcome
                        .actuations
                        .into_iter()
                        .map(|actuation| execution_wire::Actuation {
                            port: actuation.port,
                            payload: actuation.payload,
                            valid_until_ns: actuation.valid_until_ns,
                        })
                        .collect(),
                    required_deliveries: outcome
                        .required_deliveries
                        .into_iter()
                        .map(|delivery| execution_wire::DeliveryReceipt {
                            port: delivery.port,
                            direction: delivery.direction,
                            target: delivery.target.unwrap_or_default(),
                            sequence: delivery.sequence,
                            item: delivery.item,
                            bytes: delivery.bytes,
                        })
                        .collect(),
                };
                publish_execution(bus, &launch.instance_id, "accepted", &response).await?;
                last_boundary = Some(request.boundary);
                accepted.insert(request.boundary, response);
                while accepted.len() > MAX_RETAINED_CONTROLLED_INVOCATIONS {
                    let Some(oldest) = accepted.keys().next().copied() else {
                        break;
                    };
                    accepted.remove(&oldest);
                }
            }
        }
    }
}

fn next_due_boundary(previous: u64, period_ns: u64, quantum_ns: u64) -> Option<u64> {
    if period_ns == 0 || quantum_ns == 0 || !period_ns.is_multiple_of(quantum_ns) {
        return None;
    }
    previous.checked_add(period_ns / quantum_ns)
}

fn decode_execution_sample<M>(sample: &zenoh::sample::Sample) -> crate::Result<M>
where
    M: prost::Message + Default,
{
    if !execution_protocol::has_encoding(sample) {
        return Err(anyhow::anyhow!(
            "private execution message used an unexpected encoding"
        ));
    }
    execution_protocol::decode(sample.payload().to_bytes().as_ref())
}

async fn send_runtime_failure(
    bus: &crate::bus::BusHandle,
    launch: &RuntimeLaunch,
    timeline_id: &str,
    boundary: u64,
    reason: &str,
) -> crate::Result<()> {
    let failure = execution_wire::RuntimeFailure {
        execution_id: bus.execution().to_string(),
        timeline_id: timeline_id.to_owned(),
        runtime_instance: launch.instance_id.clone(),
        boundary,
        reason: reason.to_owned(),
    };
    publish_execution(bus, &launch.instance_id, "failure", &failure).await
}

fn decode_digest(value: &str) -> Option<Vec<u8>> {
    if value.len() != 64 || !value.is_ascii() {
        return None;
    }
    let mut digest = Vec::with_capacity(32);
    let bytes = value.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        digest.push(((high << 4) | low) as u8);
    }
    Some(digest)
}

/// The execution-scoped input side of the runtime process boundary.
///
/// The empty implementation still owns the live bus handle once the process
/// has joined its execution.  It is useful for runtimes with no input fields
/// and, importantly, makes bus closure a runtime failure rather than an
/// invisible source of invented defaults.
const MAX_EXPIRED_CORRELATIONS: usize = 4096;
const MAX_COMMAND_HIGH_WATERMARKS: usize = 4096;
const MAX_EXTERNAL_COMMANDS_PER_CUT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExchangeKind {
    Read,
    Request,
}

struct PendingCorrelation {
    field: &'static str,
    key: TransportValue,
    kind: ExchangeKind,
    deadline: Instant,
    expected_source: String,
}

type CorrelationKey = (String, u64);
type CorrelationMap = Arc<Mutex<BTreeMap<CorrelationKey, PendingCorrelation>>>;
type ExpiredCorrelationSet = Arc<Mutex<BTreeSet<CorrelationKey>>>;
type OperationQueue = Arc<Mutex<Vec<OperationCompletionRecord>>>;

enum ExchangeCompletion {
    Read {
        field: &'static str,
        key: TransportValue,
        result: Result<TransportValue, ReadError>,
    },
    Request {
        field: &'static str,
        key: TransportValue,
        result: Result<TransportValue, RequestError>,
    },
}

type ExchangeCompletionQueue = Arc<Mutex<Vec<ExchangeCompletion>>>;

fn not_sent_completion(
    field: &'static str,
    key: TransportValue,
    kind: ExchangeKind,
    reason: impl Into<String>,
) -> ExchangeCompletion {
    let reason = reason.into();
    match kind {
        ExchangeKind::Read => ExchangeCompletion::Read {
            field,
            key,
            result: Err(ReadError::NotSent(reason)),
        },
        ExchangeKind::Request => ExchangeCompletion::Request {
            field,
            key,
            result: Err(RequestError::NotSent(reason)),
        },
    }
}

struct ExecutionInputAdapter<R> {
    bus: Option<crate::bus::BusHandle>,
    subscriptions: Vec<BoundSubscription>,
    command_high_watermarks: BTreeMap<(String, String, String), u64>,
    external_ingress_high_watermarks: BTreeMap<String, u64>,
    future_commands: BTreeMap<&'static str, Vec<WireSample>>,
    command_ranks: BTreeMap<(String, String), u64>,
    correlations: Option<CorrelationMap>,
    expired_correlations: Option<ExpiredCorrelationSet>,
    operation_completions: Option<OperationQueue>,
    exchange_completions: Option<ExchangeCompletionQueue>,
    stream_terminal: BTreeSet<&'static str>,
    last_input_receipts: Vec<RuntimeInputReceipt>,
    stopped: bool,
    _runtime: PhantomData<fn() -> R>,
}

type RuntimeSubscription =
    zenoh::pubsub::Subscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>;

struct BoundSubscription {
    field: &'static str,
    binding: super::transport::PortBinding,
    direction: InputDirection,
    max_items: u64,
    max_bytes: u64,
    /// The direct subscriber remains available to the in-process test path.
    /// Process-bound subscriptions are drained by `delivery_receive_loop`
    /// into the receiver-owned bounded queue below.
    subscriber: Option<RuntimeSubscription>,
    delivery: Option<DeliverySubscription>,
}

/// One receiver-owned queue and its cancellation fence.
///
/// The queue is separate from Zenoh's subscriber handler.  A record is
/// acknowledged only after this queue has admitted it, so producer-side
/// publication completion cannot be mistaken for receiver admission.
struct DeliverySubscription {
    queue: Arc<Mutex<DeliveryQueue>>,
    cancel: CancellationToken,
    expected: Arc<AtomicBool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct DeliveryIdentity {
    execution_id: String,
    timeline_id: String,
    boundary: u64,
    source: String,
    target: String,
    port: String,
    direction: String,
    sequence: u64,
    item: u32,
    bytes: u64,
}

struct DeliveryQueue {
    items: VecDeque<WireSample>,
    bytes: u64,
    max_items: u64,
    max_bytes: u64,
    input_kind: super::input::InputKind,
    timeline_id: Option<String>,
    /// One high-water mark per immutable source route.  Controlled records
    /// are ordered by boundary first, then producer sequence and item.  This
    /// gives duplicate delivery idempotence without retaining every identity
    /// for the lifetime of a long-running runtime.
    high_watermarks: BTreeMap<(String, String, String, String), DeliveryIdentity>,
}

impl DeliveryQueue {
    fn new(max_items: u64, max_bytes: u64, input_kind: super::input::InputKind) -> Self {
        Self {
            items: VecDeque::new(),
            bytes: 0,
            max_items,
            max_bytes,
            input_kind,
            timeline_id: None,
            high_watermarks: BTreeMap::new(),
        }
    }

    fn set_timeline(&mut self, timeline_id: &str) {
        match self.timeline_id.as_deref() {
            None => {
                self.timeline_id = Some(timeline_id.to_owned());
                self.retain_timeline(timeline_id);
            }
            Some(current) if current != timeline_id => {
                self.items.clear();
                self.bytes = 0;
                self.high_watermarks.clear();
                self.timeline_id = Some(timeline_id.to_owned());
            }
            Some(_) => {}
        }
    }

    fn retain_timeline(&mut self, timeline_id: &str) {
        let mut bytes = 0_u64;
        self.items.retain(|sample| {
            let metadata = sample.metadata();
            let controlled = metadata.execution_id.is_some()
                || metadata.timeline_id.is_some()
                || metadata.boundary.is_some()
                || metadata.item.is_some();
            let keep = !controlled || metadata.timeline_id.as_deref() == Some(timeline_id);
            if keep {
                bytes = bytes.saturating_add(sample.payload().len() as u64);
            }
            keep
        });
        self.bytes = bytes;
        self.high_watermarks
            .retain(|_, identity| identity.timeline_id == timeline_id);
    }

    fn clear(&mut self) {
        self.items.clear();
        self.bytes = 0;
        self.high_watermarks.clear();
    }

    fn accepts_timeline(&self, timeline_id: &str) -> bool {
        self.timeline_id
            .as_deref()
            .is_some_and(|current| current == timeline_id)
    }

    fn identity(
        &self,
        sample: &WireSample,
        target: &str,
        port: &str,
        direction: &str,
    ) -> Result<DeliveryIdentity, TransportError> {
        let metadata = sample.metadata();
        Ok(DeliveryIdentity {
            execution_id: metadata.execution_id.clone().ok_or_else(|| {
                TransportError::InvalidMetadata {
                    detail: "required delivery is missing execution identity".to_owned(),
                }
            })?,
            timeline_id: metadata.timeline_id.clone().ok_or_else(|| {
                TransportError::InvalidMetadata {
                    detail: "required delivery is missing timeline identity".to_owned(),
                }
            })?,
            boundary: metadata
                .boundary
                .ok_or_else(|| TransportError::InvalidMetadata {
                    detail: "required delivery is missing boundary identity".to_owned(),
                })?,
            source: metadata
                .source
                .clone()
                .ok_or_else(|| TransportError::InvalidMetadata {
                    detail: "required delivery is missing source identity".to_owned(),
                })?,
            target: target.to_owned(),
            port: port.to_owned(),
            direction: direction.to_owned(),
            sequence: metadata
                .sequence
                .ok_or_else(|| TransportError::InvalidMetadata {
                    detail: "required delivery is missing sequence identity".to_owned(),
                })?,
            item: metadata
                .item
                .ok_or_else(|| TransportError::InvalidMetadata {
                    detail: "required delivery is missing item identity".to_owned(),
                })?,
            bytes: sample.payload().len() as u64,
        })
    }

    fn admit(
        &mut self,
        sample: WireSample,
        target: &str,
        port: &str,
        direction: &str,
    ) -> Result<(DeliveryIdentity, bool), DeliveryAdmissionError> {
        let identity = self
            .identity(&sample, target, port, direction)
            .map_err(DeliveryAdmissionError::Malformed)?;
        let bytes = identity.bytes;
        let route = identity.route_key();
        if let Some(previous) = self.high_watermarks.get(&route) {
            let order = (identity.boundary, identity.sequence, identity.item).cmp(&(
                previous.boundary,
                previous.sequence,
                previous.item,
            ));
            if order.is_le() {
                if order == std::cmp::Ordering::Equal && previous.bytes != identity.bytes {
                    return Err(DeliveryAdmissionError::Malformed(
                        TransportError::InvalidMetadata {
                            detail: "delivery identity was reused with different payload bytes"
                                .to_owned(),
                        },
                    ));
                }
                // Duplicate or stale delivery is idempotently acknowledged,
                // but it must never be exposed to the generated decoder.
                return Ok((identity, false));
            }
        }
        let is_replaceable = matches!(
            self.input_kind,
            super::input::InputKind::Latest | super::input::InputKind::Setpoint
        );
        if !is_replaceable && self.items.len() as u64 >= self.max_items {
            return Err(DeliveryAdmissionError::Saturated(format!(
                "receiver queue item capacity {} is exhausted",
                self.max_items
            )));
        }
        let next_bytes = if is_replaceable {
            bytes
        } else {
            self.bytes.checked_add(bytes).ok_or_else(|| {
                DeliveryAdmissionError::Saturated("receiver queue byte count overflowed".to_owned())
            })?
        };
        if next_bytes > self.max_bytes {
            return Err(DeliveryAdmissionError::Saturated(format!(
                "receiver queue byte capacity {} is exhausted",
                self.max_bytes
            )));
        }
        if is_replaceable {
            self.items.clear();
            self.bytes = 0;
        }
        self.bytes = next_bytes;
        self.items.push_back(sample);
        self.high_watermarks.insert(route, identity.clone());
        Ok((identity, true))
    }

    /// Admit a normal hardware or public-ingress record.  These records do
    /// not carry controlled execution identity and therefore have no private
    /// acknowledgement leg, but they still enter the same receiver-owned
    /// bounded queue and obey replacement/accumulation semantics.
    fn admit_untracked(&mut self, sample: WireSample) -> Result<(), DeliveryAdmissionError> {
        let bytes = sample.payload().len() as u64;
        let is_replaceable = matches!(
            self.input_kind,
            super::input::InputKind::Latest | super::input::InputKind::Setpoint
        );
        if !is_replaceable && self.items.len() as u64 >= self.max_items {
            return Err(DeliveryAdmissionError::Saturated(format!(
                "receiver queue item capacity {} is exhausted",
                self.max_items
            )));
        }
        let next_bytes = if is_replaceable {
            bytes
        } else {
            self.bytes.checked_add(bytes).ok_or_else(|| {
                DeliveryAdmissionError::Saturated("receiver queue byte count overflowed".to_owned())
            })?
        };
        if next_bytes > self.max_bytes {
            return Err(DeliveryAdmissionError::Saturated(format!(
                "receiver queue byte capacity {} is exhausted",
                self.max_bytes
            )));
        }
        if is_replaceable {
            self.items.clear();
        }
        self.bytes = next_bytes;
        self.items.push_back(sample);
        Ok(())
    }

    fn drain(&mut self) -> Vec<WireSample> {
        self.bytes = 0;
        self.items.drain(..).collect()
    }
}

/// Compute one receiver-owned reservation for a complete input field.  A
/// fan-in field has one aggregate budget, not one budget per producer route.
/// Latest and Setpoint inputs replace their retained value, while all other
/// input forms accumulate records until the next invocation freezes them.
fn aggregate_delivery_capacity(
    field: &super::transport::InputTransportField,
    routes: &[ResolvedInputRoute],
) -> (u64, u64) {
    let replaceable = matches!(
        field.kind,
        super::input::InputKind::Latest | super::input::InputKind::Setpoint
    );
    let max_items = field.max_items.unwrap_or_else(|| {
        if replaceable {
            1
        } else {
            routes
                .iter()
                .map(|route| route.max_items)
                .fold(0_u64, u64::saturating_add)
                .max(1)
        }
    });
    let max_bytes = field.max_bytes.unwrap_or_else(|| {
        if replaceable {
            routes
                .iter()
                .map(|route| route.max_bytes)
                .max()
                .unwrap_or(1)
        } else {
            routes
                .iter()
                .map(|route| route.max_bytes)
                .fold(0_u64, u64::saturating_add)
                .max(1)
        }
    });
    (max_items.max(1), max_bytes.max(1))
}

#[derive(Debug)]
enum DeliveryAdmissionError {
    Malformed(TransportError),
    Saturated(String),
}

struct DeliveryReceiver {
    subscriber: RuntimeSubscription,
    queue: Arc<Mutex<DeliveryQueue>>,
    bus: crate::bus::BusHandle,
    expected_source: String,
    expected_callers: BTreeSet<String>,
    target: String,
    port: String,
    direction: String,
    cancel: CancellationToken,
}

impl DeliveryIdentity {
    fn route_key(&self) -> (String, String, String, String) {
        (
            self.source.clone(),
            self.target.clone(),
            self.port.clone(),
            self.direction.clone(),
        )
    }
}

/// Drain one Zenoh port subscription into the receiver-owned queue and
/// acknowledge only after queue admission succeeds.  This task intentionally
/// runs independently of the runtime schedule: a healthy paused or not-due
/// consumer still admits graph traffic, while its later freeze consumes the
/// already admitted records.
async fn delivery_receive_loop(receiver: DeliveryReceiver) -> crate::Result<()> {
    let DeliveryReceiver {
        subscriber,
        queue,
        bus,
        expected_source,
        expected_callers,
        target,
        port,
        direction,
        cancel,
    } = receiver;
    loop {
        let sample = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            result = subscriber.recv_async() => result.map_err(|error| {
                anyhow::anyhow!(TransportError::Transport(error.to_string()))
            })?,
        };
        let wire = WireSample::from_zenoh(sample)?;
        let metadata = wire.metadata();
        let source = metadata
            .source
            .as_deref()
            .filter(|source| !source.is_empty())
            .unwrap_or(&expected_source)
            .to_owned();

        let has_controlled_identity = metadata.execution_id.is_some()
            || metadata.timeline_id.is_some()
            || metadata.boundary.is_some()
            || metadata.item.is_some();

        // Unstamped hardware traffic and authenticated supervisor ingress use
        // the normal input path.  They have no source runtime waiting for a
        // private controlled-delivery acknowledgement, but remain bounded by
        // the receiver-owned queue.  Optional traffic is dropped on local
        // saturation rather than turning a healthy legacy flow into a fatal
        // process exit.
        if !has_controlled_identity {
            if direction == "request" && source != "supervisor" {
                validate_controlled_request_source(metadata, &source, &port, &expected_callers)?;
            } else if direction != "request" && source != expected_source {
                return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!(
                        "runtime delivery source `{source}` does not match `{expected_source}`"
                    ),
                }));
            }
            let mut queue = match queue.lock() {
                Ok(queue) => queue,
                Err(poisoned) => poisoned.into_inner(),
            };
            let _ = queue.admit_untracked(wire);
            continue;
        }

        if direction == "request" {
            validate_controlled_request_source(metadata, &source, &port, &expected_callers)?;
        } else if source != expected_source {
            // A sample from a different producer cannot satisfy this route.
            // Failing the worker makes the owning bus enter its fatal state;
            // the supervisor then reports a bounded required-delivery failure
            // instead of accepting an identity-spoofed record.
            return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                detail: format!(
                    "runtime delivery source `{source}` does not match `{expected_source}`"
                ),
            }));
        }

        let identity = {
            let queue = match queue.lock() {
                Ok(queue) => queue,
                Err(poisoned) => poisoned.into_inner(),
            };
            queue.identity(&wire, &target, &port, &direction)
        }?;
        if identity.execution_id != bus.execution().to_string() {
            publish_delivery_ack(
                &bus,
                &identity,
                false,
                Some("delivery belongs to another execution".to_owned()),
            )
            .await?;
            continue;
        }
        let timeline_matches = {
            let queue = match queue.lock() {
                Ok(queue) => queue,
                Err(poisoned) => poisoned.into_inner(),
            };
            queue.accepts_timeline(&identity.timeline_id)
        };
        if !timeline_matches {
            publish_delivery_ack(
                &bus,
                &identity,
                false,
                Some("delivery belongs to a retired timeline".to_owned()),
            )
            .await?;
            continue;
        }

        let result = {
            let mut queue = match queue.lock() {
                Ok(queue) => queue,
                Err(poisoned) => poisoned.into_inner(),
            };
            queue.admit(wire, &target, &port, &direction)
        };
        match result {
            Ok((identity, _inserted)) => {
                publish_delivery_ack(&bus, &identity, true, None).await?;
            }
            Err(DeliveryAdmissionError::Saturated(detail)) => {
                publish_delivery_ack(&bus, &identity, false, Some(detail)).await?;
            }
            Err(DeliveryAdmissionError::Malformed(error)) => {
                return Err(anyhow::anyhow!(error));
            }
        }
    }
}

fn validate_controlled_request_source(
    metadata: &super::transport::RuntimeWireMetadata,
    source: &str,
    port: &str,
    expected_callers: &BTreeSet<String>,
) -> crate::Result<()> {
    if source == "supervisor" {
        return Ok(());
    }
    let caller = metadata
        .caller
        .as_deref()
        .filter(|caller| !caller.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(TransportError::CommandCorrelation(format!(
                "request delivery to `{port}` is missing caller identity"
            )))
        })?;
    let (caller_instance, _caller_field) = parse_graph_endpoint(caller).map_err(|error| {
        anyhow::anyhow!(TransportError::CommandCorrelation(format!(
            "request delivery caller `{caller}` is invalid: {error}"
        )))
    })?;
    if caller_instance != source || !expected_callers.contains(caller) {
        return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
            format!("request delivery source `{source}` is not an admitted caller for `{port}`")
        )));
    }
    Ok(())
}

async fn publish_delivery_ack(
    bus: &crate::bus::BusHandle,
    identity: &DeliveryIdentity,
    admitted: bool,
    detail: Option<String>,
) -> crate::Result<()> {
    let ack = execution_wire::DeliveryAck {
        execution_id: identity.execution_id.clone(),
        timeline_id: identity.timeline_id.clone(),
        boundary: identity.boundary,
        source: identity.source.clone(),
        target: identity.target.clone(),
        port: identity.port.clone(),
        direction: identity.direction.clone(),
        sequence: identity.sequence,
        item: identity.item,
        bytes: identity.bytes,
        admitted,
        detail,
    };
    let key = execution_protocol::key(bus, &identity.source, "delivery-ack");
    let payload = execution_protocol::encode(&ack)?;
    bus.session()?
        .put(key, payload)
        .encoding(Encoding::from(
            execution_protocol::PROTOBUF_ENCODING.to_owned(),
        ))
        .await
        .map_err(|error| anyhow::anyhow!(TransportError::Transport(error.to_string())))?;
    Ok(())
}

struct CollectedInput {
    field: &'static str,
    binding: super::transport::PortBinding,
    direction: InputDirection,
    max_items: u64,
    max_bytes: u64,
    samples: Vec<WireSample>,
}

struct ErasedManagedOperation {
    operation: ManagedOperation<u64, TransportValue, TransportValue, OperationWorker>,
    keys: BTreeMap<u64, TransportValue>,
    pending_key: Option<u64>,
}

struct ReadSubscription {
    field: &'static str,
    binding: super::transport::PortBinding,
    signature: crate::port::PortSignature,
    max_request_bytes: u64,
    allowed_callers: BTreeMap<String, u64>,
    subscriber: RuntimeSubscription,
}

impl<R> ExecutionInputAdapter<R> {
    fn unbound() -> Self {
        Self {
            bus: None,
            subscriptions: Vec::new(),
            command_high_watermarks: BTreeMap::new(),
            external_ingress_high_watermarks: BTreeMap::new(),
            future_commands: BTreeMap::new(),
            command_ranks: BTreeMap::new(),
            correlations: None,
            expired_correlations: None,
            operation_completions: None,
            exchange_completions: None,
            stream_terminal: BTreeSet::new(),
            last_input_receipts: Vec::new(),
            stopped: false,
            _runtime: PhantomData,
        }
    }

    fn with_shared_state(
        mut self,
        correlations: CorrelationMap,
        expired_correlations: ExpiredCorrelationSet,
        operation_completions: OperationQueue,
        exchange_completions: ExchangeCompletionQueue,
    ) -> Self {
        self.correlations = Some(correlations);
        self.expired_correlations = Some(expired_correlations);
        self.operation_completions = Some(operation_completions);
        self.exchange_completions = Some(exchange_completions);
        self
    }

    async fn bind(
        &mut self,
        bus: crate::bus::BusHandle,
        manifest: &RuntimeLaunchManifest,
    ) -> crate::Result<()>
    where
        R: RegisteredRuntime,
        R::Inputs: TransportInputSet,
    {
        let fields = <R::Inputs as TransportInputSet>::transport_fields();
        let session = bus.session()?;
        let mut subscriptions = Vec::new();
        let command_ranks = manifest.command_ranks(&manifest.instance_id)?;
        for field in fields {
            let routes = manifest.input_routes(field)?;
            if routes.is_empty() {
                continue;
            }
            let (queue_max_items, queue_max_bytes) = aggregate_delivery_capacity(field, &routes);
            let queue = Arc::new(Mutex::new(DeliveryQueue::new(
                queue_max_items,
                queue_max_bytes,
                field.kind,
            )));
            for route in routes {
                let key = bus.full_key(&transport::port_key(
                    &route.source_instance,
                    &route.source_port,
                    route.direction.key_direction(),
                ));
                let key_expr =
                    zenoh::key_expr::OwnedKeyExpr::new(key.clone()).map_err(|error| {
                        anyhow::anyhow!(TransportError::Transport(format!(
                            "invalid generated Runtime input key `{key}`: {error}"
                        )))
                    })?;
                let capacity = usize::try_from(route.max_items).map_err(|_| {
                    anyhow::anyhow!(TransportError::BatchTooLarge {
                        port: route.binding.name.clone(),
                        what: "item count",
                        actual: route.max_items,
                        maximum: usize::MAX as u64,
                    })
                })?;
                let subscriber = session
                    .declare_subscriber(key_expr)
                    .with(zenoh::handlers::FifoChannel::new(capacity))
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!(TransportError::Transport(error.to_string()))
                    })?;
                let cancel = CancellationToken::new();
                let expected = Arc::new(AtomicBool::new(false));
                let worker_queue = Arc::clone(&queue);
                let worker_cancel = cancel.clone();
                let worker_expected = Arc::clone(&expected);
                let worker_bus = bus.clone();
                let worker_source = route.source_instance.clone();
                let worker_callers = if route.direction == InputDirection::Request {
                    command_ranks
                        .iter()
                        .filter(|((port, _caller), _rank)| port == &route.binding.name)
                        .map(|((_port, caller), _rank)| caller.clone())
                        .collect()
                } else {
                    BTreeSet::new()
                };
                let worker_target = manifest.instance_id.clone();
                let worker_port = route.binding.name.clone();
                let worker_direction = route.direction.key_direction().to_owned();
                let worker = tokio::spawn(async move {
                    let result = delivery_receive_loop(DeliveryReceiver {
                        subscriber,
                        queue: worker_queue,
                        bus: worker_bus,
                        expected_source: worker_source,
                        expected_callers: worker_callers,
                        target: worker_target,
                        port: worker_port,
                        direction: worker_direction,
                        cancel: worker_cancel,
                    })
                    .await;
                    if let Err(error) = result {
                        panic!("runtime delivery receiver failed: {error:#}");
                    }
                    // A normal return is expected only after the owner has
                    // fenced this worker during stop or reset.
                    if !worker_expected.load(Ordering::Acquire) {
                        panic!("runtime delivery receiver exited without cancellation");
                    }
                });
                if let Err(worker) = bus.register_named_worker(
                    format!(
                        "delivery-receiver-{}-{}",
                        route.source_instance, route.binding.name
                    ),
                    Arc::clone(&expected),
                    worker,
                ) {
                    worker.abort();
                    return Err(anyhow::anyhow!(TransportError::Transport(
                        "runtime delivery receiver could not be registered".to_owned(),
                    )));
                }
                subscriptions.push(BoundSubscription {
                    field: route.field,
                    binding: route.binding,
                    direction: route.direction,
                    max_items: queue_max_items,
                    max_bytes: queue_max_bytes,
                    subscriber: None,
                    delivery: Some(DeliverySubscription {
                        queue: Arc::clone(&queue),
                        cancel,
                        expected,
                    }),
                });
            }
        }
        self.command_ranks = command_ranks;
        self.bus = Some(bus);
        self.subscriptions = subscriptions;
        Ok(())
    }

    #[cfg(test)]
    async fn bind_direct(&mut self, bus: crate::bus::BusHandle, instance: &str) -> crate::Result<()>
    where
        R: RegisteredRuntime,
        R::Inputs: TransportInputSet,
    {
        let session = bus.session()?;
        let mut subscriptions = Vec::new();
        for field in <R::Inputs as TransportInputSet>::transport_fields() {
            let Some(signature) = field.signature else {
                continue;
            };
            let direction = InputDirection::for_kind(field.kind)?;
            let max_items = field.max_items.unwrap_or(1);
            let max_bytes = field.max_bytes.unwrap_or(u64::MAX);
            let key = bus.full_key(&transport::port_key(
                instance,
                signature.name,
                direction.key_direction(),
            ));
            let key_expr = zenoh::key_expr::OwnedKeyExpr::new(key.clone()).map_err(|error| {
                anyhow::anyhow!(TransportError::Transport(format!(
                    "invalid generated Runtime input key `{key}`: {error}"
                )))
            })?;
            let capacity = usize::try_from(max_items).map_err(|_| {
                anyhow::anyhow!(TransportError::BatchTooLarge {
                    port: signature.name.to_owned(),
                    what: "item count",
                    actual: max_items,
                    maximum: usize::MAX as u64,
                })
            })?;
            let subscriber = session
                .declare_subscriber(key_expr)
                .with(zenoh::handlers::FifoChannel::new(capacity))
                .await
                .map_err(|error| anyhow::anyhow!(TransportError::Transport(error.to_string())))?;
            subscriptions.push(BoundSubscription {
                field: field.name,
                binding: super::transport::PortBinding::from_signature(signature),
                direction,
                max_items,
                max_bytes,
                subscriber: Some(subscriber),
                delivery: None,
            });
        }
        self.bus = Some(bus);
        self.subscriptions = subscriptions;
        Ok(())
    }

    fn ensure_open(&self) -> crate::Result<()> {
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(crate::bus::BusError::Closed))?;
        if matches!(bus.terminal(), crate::bus::BusTerminal::Open) {
            Ok(())
        } else {
            Err(anyhow::anyhow!(crate::bus::BusError::Closed))
        }
    }

    fn validate_stream_lifecycle(
        &self,
        field: &'static str,
        samples: &[WireSample],
    ) -> crate::Result<bool> {
        if self.stream_terminal.contains(field) {
            return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                detail: format!("stream input `{field}` received data after terminal control"),
            }));
        }
        let mut terminal = false;
        for sample in samples {
            let control = sample.metadata().wire_control()?;
            if terminal {
                return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!(
                        "stream input `{field}` received {:?} after a terminal control",
                        control
                    ),
                }));
            }
            if matches!(
                control,
                super::transport::WireControl::End | super::transport::WireControl::Failed
            ) {
                terminal = true;
            }
        }
        Ok(terminal)
    }

    fn validate_command_record(
        &self,
        batch: &CollectedInput,
        sample: &WireSample,
    ) -> crate::Result<(transport::CommandIngress, String, String, u64)>
    where
        R: RegisteredRuntime,
        R::Inputs: TransportInputSet,
    {
        if sample.metadata().wire_control()? != transport::WireControl::Data {
            return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                "command request used a stream control record".to_owned(),
            )));
        }
        let metadata = sample.metadata();
        let id = metadata.command_id.ok_or_else(|| {
            anyhow::anyhow!(TransportError::CommandCorrelation(
                "correlated Runtime record is missing command_id".to_owned(),
            ))
        })?;
        let _eligible_boundary = metadata.eligible_boundary.ok_or_else(|| {
            anyhow::anyhow!(TransportError::CommandCorrelation(
                "correlated Runtime record is missing eligible_boundary".to_owned(),
            ))
        })?;
        let source = metadata
            .source
            .clone()
            .filter(|source| !source.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(TransportError::CommandCorrelation(
                    "correlated Runtime record is missing source".to_owned(),
                ))
            })?;
        let caller = metadata
            .caller
            .clone()
            .filter(|caller| !caller.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(TransportError::CommandCorrelation(
                    "correlated Runtime record is missing caller identity".to_owned(),
                ))
            })?;
        let ingress = transport::command_ingress(metadata)?;
        match ingress {
            transport::CommandIngress::Controlled { caller_rank } => {
                let (caller_instance, _caller_field) =
                    parse_graph_endpoint(&caller).map_err(|error| {
                        anyhow::anyhow!(TransportError::CommandCorrelation(format!(
                            "invalid caller identity `{caller}`: {error}"
                        )))
                    })?;
                if caller_instance != source {
                    return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                        format!("caller `{caller}` does not match source `{source}`"),
                    )));
                }
                if let Some(expected) = self
                    .command_ranks
                    .get(&(batch.binding.name.clone(), caller.clone()))
                    && *expected != caller_rank
                {
                    return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                        format!(
                            "source `{source}` used caller rank {caller_rank}, expected {expected}"
                        ),
                    )));
                } else if !self.command_ranks.is_empty()
                    && !self
                        .command_ranks
                        .contains_key(&(batch.binding.name.clone(), caller.clone()))
                {
                    return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                        format!(
                            "caller `{caller}` is not connected to Commands port `{}`",
                            batch.binding.name
                        ),
                    )));
                }
            }
            transport::CommandIngress::External { .. } => {
                if source != "supervisor" || caller != "supervisor.public" {
                    return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                        "external command identity is not supervisor-owned".to_owned(),
                    )));
                }
            }
        }
        let signature = <R::Inputs as TransportInputSet>::transport_fields()
            .iter()
            .find(|field| field.name == batch.field)
            .and_then(|field| field.signature)
            .ok_or_else(|| {
                anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!(
                        "Commands input `{}` has no generated descriptor",
                        batch.field
                    ),
                })
            })?;
        transport::validate_binding_identity(&batch.binding, signature)?;
        transport::validate_request(signature, sample, batch.max_bytes)?;
        Ok((ingress, source, caller, id))
    }

    fn validate_future_command_admission(&self, batch: &CollectedInput) -> crate::Result<()>
    where
        R: RegisteredRuntime,
        R::Inputs: TransportInputSet,
    {
        let mut identities = BTreeSet::new();
        let mut external_sequences = BTreeSet::new();
        if let Some(retained) = self.future_commands.get(batch.field) {
            for sample in retained {
                let metadata = sample.metadata();
                let id = metadata.command_id.ok_or_else(|| {
                    anyhow::anyhow!(TransportError::CommandCorrelation(
                        "retained command is missing command_id".to_owned(),
                    ))
                })?;
                let source = metadata.source.as_deref().unwrap_or_default();
                let caller = metadata.caller.as_deref().unwrap_or_default();
                if !identities.insert((source.to_owned(), caller.to_owned(), id)) {
                    return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                        format!("duplicate retained command id {id} from caller `{caller}`"),
                    )));
                }
                if let transport::CommandIngress::External { ingress_sequence } =
                    transport::command_ingress(metadata)?
                {
                    external_sequences.insert(ingress_sequence);
                }
            }
        }
        for sample in &batch.samples {
            let (ingress, source, caller, id) = self.validate_command_record(batch, sample)?;
            if !identities.insert((source.clone(), caller.clone(), id)) {
                return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                    format!("duplicate command id {id} from caller `{caller}`"),
                )));
            }
            let key = (batch.field.to_owned(), source.clone(), caller.clone());
            if self
                .command_high_watermarks
                .get(&key)
                .is_some_and(|previous| id <= *previous)
            {
                return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                    format!("stale or replayed correlation id {id}"),
                )));
            }
            if let transport::CommandIngress::External { ingress_sequence } = ingress
                && (self
                    .external_ingress_high_watermarks
                    .get(batch.field)
                    .is_some_and(|previous| ingress_sequence <= *previous)
                    || !external_sequences.insert(ingress_sequence))
            {
                return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                    format!("stale or replayed external ingress sequence {ingress_sequence}"),
                )));
            }
        }
        Ok(())
    }
}

impl<R> TransportKeyLookup for ExecutionInputAdapter<R> {
    fn take_key(&mut self, field: &str, command_id: u64) -> Option<TransportValue> {
        let map = self.correlations.as_ref()?;
        let mut map = match map.lock() {
            Ok(map) => map,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.remove(&(field.to_owned(), command_id))
            .map(|pending| pending.key)
    }

    fn validate_reply(
        &mut self,
        field: &str,
        command_id: u64,
        sample: &WireSample,
    ) -> crate::Result<()> {
        let source = sample
            .metadata()
            .source
            .as_deref()
            .filter(|source| !source.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(TransportError::CommandCorrelation(
                    "correlated reply is missing source".to_owned(),
                ))
            })?;
        let correlations = self.correlations.as_ref().ok_or_else(|| {
            anyhow::anyhow!(TransportError::Transport(
                "activation correlation table is not bound".to_owned(),
            ))
        })?;
        let correlations = match correlations.lock() {
            Ok(correlations) => correlations,
            Err(poisoned) => poisoned.into_inner(),
        };
        let pending = correlations
            .get(&(field.to_owned(), command_id))
            .ok_or_else(|| {
                anyhow::anyhow!(TransportError::CommandCorrelation(format!(
                    "stale or unknown reply correlation id {command_id}"
                )))
            })?;
        if pending.expected_source != source {
            return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                format!(
                    "reply correlation id {command_id} came from `{source}`, expected `{}`",
                    pending.expected_source
                ),
            )));
        }
        Ok(())
    }

    fn is_expired(&mut self, field: &str, command_id: u64) -> bool {
        let Some(expired) = &self.expired_correlations else {
            return false;
        };
        let expired = match expired.lock() {
            Ok(expired) => expired,
            Err(poisoned) => poisoned.into_inner(),
        };
        expired.contains(&(field.to_owned(), command_id))
    }
}

impl<R> InputSource<R> for ExecutionInputAdapter<R>
where
    R: RegisteredRuntime,
    R::Inputs: TransportInputSet + super::input::TransportInputSink,
{
    fn freeze(&mut self, _candidate: &HardwareInvocation) -> crate::Result<R::Inputs> {
        if self.stopped {
            return Err(anyhow::anyhow!(crate::bus::BusError::Closed));
        }
        self.ensure_open()?;
        self.last_input_receipts.clear();
        let mut input_receipts = BTreeMap::<(String, String), RuntimeInputReceipt>::new();
        let mut inputs = R::Inputs::empty();
        <R::Inputs as TransportInputSet>::expire_transport_fields_at(
            &mut inputs,
            _candidate.context().now(),
        )?;
        if let Some(queue) = &self.operation_completions {
            let completions = {
                let mut queue = match queue.lock() {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
                std::mem::take(&mut *queue)
            };
            for completion in completions {
                <R::Inputs as super::input::TransportInputSink>::set_operation(
                    &mut inputs,
                    completion.field,
                    completion.key,
                    completion.result,
                )?;
            }
        }
        if let Some(queue) = &self.exchange_completions {
            let completions = {
                let mut queue = match queue.lock() {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
                std::mem::take(&mut *queue)
            };
            for completion in completions {
                match completion {
                    ExchangeCompletion::Read { field, key, result } => {
                        <R::Inputs as super::input::TransportInputSink>::set_read(
                            &mut inputs,
                            field,
                            key,
                            result,
                        )?
                    }
                    ExchangeCompletion::Request { field, key, result } => {
                        <R::Inputs as super::input::TransportInputSink>::set_request(
                            &mut inputs,
                            field,
                            key,
                            result,
                        )?
                    }
                }
            }
        }
        let mut batches: Vec<CollectedInput> = Vec::new();
        for subscription in &self.subscriptions {
            let samples = if let Some(delivery) = &subscription.delivery {
                let mut queue = match delivery.queue.lock() {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
                queue.drain()
            } else {
                let mut samples = Vec::new();
                let Some(subscriber) = &subscription.subscriber else {
                    return Err(anyhow::anyhow!(TransportError::Transport(
                        "runtime input subscription has no receiver".to_owned(),
                    )));
                };
                loop {
                    match subscriber.try_recv() {
                        Ok(Some(sample)) => samples.push(WireSample::from_zenoh(sample)?),
                        Ok(None) => break,
                        Err(error) => {
                            return Err(anyhow::anyhow!(TransportError::Transport(
                                error.to_string(),
                            )));
                        }
                    }
                }
                samples
            };
            for sample in &samples {
                if sample.metadata().wire_control()? != transport::WireControl::Data {
                    continue;
                }
                let Some(sequence) = sample.metadata().sequence else {
                    continue;
                };
                let Some(source) = sample
                    .metadata()
                    .source
                    .as_deref()
                    .filter(|source| !source.is_empty())
                else {
                    continue;
                };
                let key = (source.to_owned(), subscription.binding.name.clone());
                let receipt = input_receipts
                    .entry(key)
                    .or_insert_with(|| RuntimeInputReceipt {
                        source: source.to_owned(),
                        port: subscription.binding.name.clone(),
                        sequence,
                        items: 0,
                        bytes: 0,
                    });
                receipt.sequence = sequence;
                receipt.items = receipt.items.saturating_add(1);
                receipt.bytes = receipt.bytes.saturating_add(sample.payload().len() as u64);
            }
            let has_retained_commands = subscription.binding.kind
                == crate::port::PortKind::Commands
                && subscription.direction == InputDirection::Request
                && self
                    .future_commands
                    .get(subscription.field)
                    .is_some_and(|retained| !retained.is_empty());
            if samples.is_empty() && !has_retained_commands {
                continue;
            }
            if let Some(batch) = batches
                .iter_mut()
                .find(|batch| batch.field == subscription.field)
            {
                if batch.binding != subscription.binding
                    || batch.direction != subscription.direction
                {
                    return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                        detail: format!(
                            "input field `{}` received conflicting source bindings",
                            subscription.field
                        ),
                    }));
                }
                batch.samples.extend(samples);
            } else {
                batches.push(CollectedInput {
                    field: subscription.field,
                    binding: subscription.binding.clone(),
                    direction: subscription.direction,
                    max_items: subscription.max_items,
                    max_bytes: subscription.max_bytes,
                    samples,
                });
            }
        }
        let current_boundary = _candidate.context().invocation_index();
        let mut future_updates = BTreeMap::new();
        for mut batch in batches {
            if batch.binding.kind == crate::port::PortKind::Commands
                && batch.direction == InputDirection::Request
            {
                self.validate_future_command_admission(&batch)?;
                let retained_before = self
                    .future_commands
                    .get(&batch.field)
                    .cloned()
                    .unwrap_or_default();
                let pending_count = retained_before
                    .len()
                    .checked_add(batch.samples.len())
                    .ok_or_else(|| {
                        anyhow::anyhow!(TransportError::BatchTooLarge {
                            port: batch.binding.name.clone(),
                            what: "future command count",
                            actual: u64::MAX,
                            maximum: batch.max_items,
                        })
                    })?;
                let pending_bytes = retained_before
                    .iter()
                    .chain(batch.samples.iter())
                    .try_fold(0_u64, |total, sample| {
                        total.checked_add(sample.payload().len() as u64)
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!(TransportError::BatchTooLarge {
                            port: batch.binding.name.clone(),
                            what: "future command encoded bytes",
                            actual: u64::MAX,
                            maximum: batch.max_bytes,
                        })
                    })?;
                if pending_count as u64 > batch.max_items {
                    return Err(anyhow::anyhow!(TransportError::BatchTooLarge {
                        port: batch.binding.name.clone(),
                        what: "future command capacity",
                        actual: pending_count as u64,
                        maximum: batch.max_items,
                    }));
                }
                if pending_bytes > batch.max_bytes {
                    return Err(anyhow::anyhow!(TransportError::BatchTooLarge {
                        port: batch.binding.name.clone(),
                        what: "future command encoded bytes",
                        actual: pending_bytes,
                        maximum: batch.max_bytes,
                    }));
                }
                let mut pending = retained_before;
                pending.extend(batch.samples);
                let mut selected = Vec::with_capacity(pending.len());
                let mut retained = Vec::new();
                for sample in pending {
                    let eligible_boundary =
                        sample.metadata().eligible_boundary.ok_or_else(|| {
                            anyhow::anyhow!(TransportError::CommandCorrelation(
                                "correlated Runtime record is missing eligible_boundary".to_owned(),
                            ))
                        })?;
                    if eligible_boundary > current_boundary {
                        retained.push(sample);
                    } else {
                        selected.push(sample);
                    }
                }
                future_updates.insert(batch.field, retained);
                batch.samples = selected;
                if batch.samples.is_empty() {
                    continue;
                }
            }
            if batch.samples.len() as u64 > batch.max_items {
                return Err(anyhow::anyhow!(TransportError::BatchTooLarge {
                    port: batch.binding.name.clone(),
                    what: "item count",
                    actual: batch.samples.len() as u64,
                    maximum: batch.max_items,
                }));
            }
            let encoded_bytes = batch
                .samples
                .iter()
                .try_fold(0_u64, |total, sample| {
                    total.checked_add(sample.payload().len() as u64)
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(TransportError::BatchTooLarge {
                        port: batch.binding.name.clone(),
                        what: "encoded bytes",
                        actual: u64::MAX,
                        maximum: batch.max_bytes,
                    })
                })?;
            if encoded_bytes > batch.max_bytes {
                return Err(anyhow::anyhow!(TransportError::BatchTooLarge {
                    port: batch.binding.name.clone(),
                    what: "encoded bytes",
                    actual: encoded_bytes,
                    maximum: batch.max_bytes,
                }));
            }
            let stream_terminal = if batch.binding.kind == crate::port::PortKind::Stream {
                Some(self.validate_stream_lifecycle(batch.field, &batch.samples)?)
            } else {
                None
            };
            let command_marks = if batch.binding.kind == crate::port::PortKind::Commands
                && batch.direction == InputDirection::Request
            {
                let mut marks = Vec::with_capacity(batch.samples.len());
                let mut external_marks = Vec::new();
                let mut batch_seen = BTreeSet::new();
                for sample in &batch.samples {
                    let metadata = sample.metadata();
                    let id = metadata.command_id.ok_or_else(|| {
                        anyhow::anyhow!(TransportError::CommandCorrelation(
                            "correlated Runtime record is missing command_id".to_owned(),
                        ))
                    })?;
                    let source = metadata
                        .source
                        .clone()
                        .filter(|source| !source.is_empty())
                        .ok_or_else(|| {
                            anyhow::anyhow!(TransportError::CommandCorrelation(
                                "correlated Runtime record is missing source".to_owned(),
                            ))
                        })?;
                    let caller = metadata
                        .caller
                        .clone()
                        .filter(|caller| !caller.is_empty())
                        .ok_or_else(|| {
                            anyhow::anyhow!(TransportError::CommandCorrelation(
                                "correlated Runtime record is missing caller identity".to_owned(),
                            ))
                        })?;
                    match transport::command_ingress(metadata)? {
                        transport::CommandIngress::Controlled { caller_rank } => {
                            let (caller_instance, _caller_field) = parse_graph_endpoint(&caller)
                                .map_err(|error| {
                                    anyhow::anyhow!(TransportError::CommandCorrelation(format!(
                                        "invalid caller identity `{caller}`: {error}"
                                    ),))
                                })?;
                            if caller_instance != source {
                                return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                                    format!("caller `{caller}` does not match source `{source}"),
                                )));
                            }
                            if let Some(expected) = self
                                .command_ranks
                                .get(&(batch.binding.name.clone(), caller.clone()))
                                && *expected != caller_rank
                            {
                                return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                                    format!(
                                        "source `{source}` used caller rank {caller_rank}, expected {expected}"
                                    ),
                                )));
                            } else if !self.command_ranks.is_empty()
                                && !self
                                    .command_ranks
                                    .contains_key(&(batch.binding.name.clone(), caller.clone()))
                            {
                                return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                                    format!(
                                        "caller `{caller}` is not connected to Commands port `{}`",
                                        batch.binding.name
                                    ),
                                )));
                            }
                        }
                        transport::CommandIngress::External { ingress_sequence } => {
                            if source != "supervisor" || caller != "supervisor.public" {
                                return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                                    "external command identity is not supervisor-owned".to_owned(),
                                )));
                            }
                            if external_marks.len() >= MAX_EXTERNAL_COMMANDS_PER_CUT {
                                return Err(anyhow::anyhow!(TransportError::BatchTooLarge {
                                    port: batch.binding.name.clone(),
                                    what: "external command count",
                                    actual: external_marks.len() as u64 + 1,
                                    maximum: MAX_EXTERNAL_COMMANDS_PER_CUT as u64,
                                }));
                            }
                            if self
                                .external_ingress_high_watermarks
                                .get(batch.field)
                                .is_some_and(|previous| ingress_sequence <= *previous)
                                || external_marks.iter().any(|(field, previous)| {
                                    field == batch.field && ingress_sequence <= *previous
                                })
                            {
                                return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                                    format!(
                                        "stale or replayed external ingress sequence {ingress_sequence}"
                                    ),
                                )));
                            }
                            external_marks.push((batch.field.to_owned(), ingress_sequence));
                        }
                    }
                    let key = (batch.field.to_owned(), source, caller);
                    if !batch_seen.insert((key.clone(), id)) {
                        return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                            format!("duplicate correlation id {id} in one input cut"),
                        )));
                    }
                    if self
                        .command_high_watermarks
                        .get(&key)
                        .is_some_and(|previous| id <= *previous)
                        || marks
                            .iter()
                            .any(|(candidate, previous)| candidate == &key && id <= *previous)
                    {
                        return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                            format!("stale or replayed correlation id {id}"),
                        )));
                    }
                    marks.push((key, id));
                }
                Some((marks, external_marks))
            } else {
                None
            };
            <R::Inputs as TransportInputSet>::decode_transport_field_with_keys_at(
                &mut inputs,
                batch.field,
                Some(&batch.binding),
                batch.samples,
                _candidate.context().now(),
                self,
            )?;
            if let Some((marks, external_marks)) = command_marks {
                if self.command_high_watermarks.len()
                    + marks
                        .iter()
                        .filter(|(key, _)| !self.command_high_watermarks.contains_key(key))
                        .count()
                    > MAX_COMMAND_HIGH_WATERMARKS
                {
                    return Err(anyhow::anyhow!(TransportError::BatchTooLarge {
                        port: batch.binding.name.clone(),
                        what: "command caller count",
                        actual: (self.command_high_watermarks.len() + marks.len()) as u64,
                        maximum: MAX_COMMAND_HIGH_WATERMARKS as u64,
                    }));
                }
                for (key, id) in marks {
                    self.command_high_watermarks.insert(key, id);
                }
                for (field, ingress_sequence) in external_marks {
                    self.external_ingress_high_watermarks
                        .insert(field, ingress_sequence);
                }
            }
            if stream_terminal == Some(true) {
                self.stream_terminal.insert(batch.field);
            }
        }
        for (field, retained) in future_updates {
            if retained.is_empty() {
                self.future_commands.remove(&field);
            } else {
                self.future_commands.insert(field, retained);
            }
        }
        self.last_input_receipts = input_receipts.into_values().collect();
        Ok(inputs)
    }

    fn take_input_receipts(&mut self) -> Vec<RuntimeInputReceipt> {
        std::mem::take(&mut self.last_input_receipts)
    }

    fn set_timeline(&mut self, timeline_id: &str) -> crate::Result<()> {
        if timeline_id.is_empty() {
            return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                detail: "runtime delivery timeline cannot be empty".to_owned(),
            }));
        }
        for subscription in &self.subscriptions {
            if let Some(delivery) = &subscription.delivery {
                let mut queue = match delivery.queue.lock() {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
                queue.set_timeline(timeline_id);
            }
        }
        Ok(())
    }

    fn stop(&mut self) -> crate::Result<()> {
        self.stopped = true;
        for subscription in &self.subscriptions {
            if let Some(delivery) = &subscription.delivery {
                delivery.expected.store(true, Ordering::Release);
                delivery.cancel.cancel();
                if let Ok(mut queue) = delivery.queue.lock() {
                    queue.clear();
                } else if let Err(poisoned) = delivery.queue.lock() {
                    poisoned.into_inner().clear();
                }
            }
        }
        self.subscriptions.clear();
        self.command_high_watermarks.clear();
        self.external_ingress_high_watermarks.clear();
        self.future_commands.clear();
        self.stream_terminal.clear();
        self.last_input_receipts.clear();
        self.command_ranks.clear();
        self.bus = None;
        Ok(())
    }

    fn reset(&mut self) -> crate::Result<()> {
        self.stopped = false;
        self.command_high_watermarks.clear();
        self.external_ingress_high_watermarks.clear();
        self.future_commands.clear();
        self.stream_terminal.clear();
        self.last_input_receipts.clear();
        for subscription in &self.subscriptions {
            if let Some(delivery) = &subscription.delivery {
                let mut queue = match delivery.queue.lock() {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
                queue.clear();
            }
        }
        if self.bus.is_none() || self.subscriptions.is_empty() {
            return Err(anyhow::anyhow!(TransportError::Transport(
                "Runtime input subscriptions are not bound after reset".to_owned(),
            )));
        }
        Ok(())
    }
}

/// One output reservation owns both encoded records and the staged side
/// effects that are dispatched only after the invocation has been accepted.
struct OutputReservation {
    records: Vec<PreparedOutput>,
    activations: Vec<StagedActivation>,
}

struct StagedActivation {
    field: &'static str,
    key: Option<TransportValue>,
    request: Option<TransportValue>,
    worker: Option<OperationWorker>,
    request_output: Option<PreparedOutput>,
    timeout_ms: Option<u64>,
    refresh_every_steps: Option<u64>,
    invocation_index: Option<u64>,
    cancel_grace_ms: Option<u64>,
    command_id: Option<u64>,
    correlation_kind: Option<ExchangeKind>,
    expected_source: Option<String>,
    local_completion: Option<ExchangeCompletion>,
}

/// The execution-scoped output side of the runtime process boundary.
struct ExecutionOutputAdapter<R> {
    bus: Option<crate::bus::BusHandle>,
    instance: Option<String>,
    context: Option<super::StepContext>,
    projections: Vec<PreparedOutput>,
    read_subscriptions: Vec<ReadSubscription>,
    pending_reads: Vec<RuntimeReadRequest>,
    activation_routes: BTreeMap<&'static str, ResolvedInputRoute>,
    staged: Vec<StagedActivation>,
    operations: BTreeMap<&'static str, ErasedManagedOperation>,
    correlations: Option<CorrelationMap>,
    expired_correlations: Option<ExpiredCorrelationSet>,
    operation_completions: Option<OperationQueue>,
    exchange_completions: Option<ExchangeCompletionQueue>,
    next_refresh_steps: BTreeMap<&'static str, u64>,
    last_state_values: BTreeMap<&'static str, ChangeToken>,
    external_read_high_watermarks: BTreeMap<&'static str, u64>,
    next_command_id: u64,
    last_product_receipts: Vec<RuntimeProductReceipt>,
    last_delivery_receipts: Vec<RuntimeDeliveryReceipt>,
    last_actuations: Vec<RuntimeActuation>,
    delivery_context: Option<(u64, String)>,
    stopped: bool,
    _runtime: PhantomData<fn() -> R>,
}

impl<R> ExecutionOutputAdapter<R> {
    fn unbound() -> Self {
        Self {
            bus: None,
            instance: None,
            context: None,
            projections: Vec::new(),
            read_subscriptions: Vec::new(),
            pending_reads: Vec::new(),
            activation_routes: BTreeMap::new(),
            staged: Vec::new(),
            operations: BTreeMap::new(),
            correlations: None,
            expired_correlations: None,
            operation_completions: None,
            exchange_completions: None,
            next_refresh_steps: BTreeMap::new(),
            last_state_values: BTreeMap::new(),
            external_read_high_watermarks: BTreeMap::new(),
            next_command_id: 1,
            last_product_receipts: Vec::new(),
            last_delivery_receipts: Vec::new(),
            last_actuations: Vec::new(),
            delivery_context: None,
            stopped: false,
            _runtime: PhantomData,
        }
    }

    fn with_shared_state(
        mut self,
        correlations: CorrelationMap,
        expired_correlations: ExpiredCorrelationSet,
        operation_completions: OperationQueue,
        exchange_completions: ExchangeCompletionQueue,
    ) -> Self {
        self.correlations = Some(correlations);
        self.expired_correlations = Some(expired_correlations);
        self.operation_completions = Some(operation_completions);
        self.exchange_completions = Some(exchange_completions);
        self
    }

    fn filter_state_projections(
        &mut self,
        context: super::StepContext,
        bootstrap: bool,
    ) -> crate::Result<()>
    where
        R: OutputBindings,
    {
        let mut retained = Vec::with_capacity(self.projections.len());
        for output in std::mem::take(&mut self.projections) {
            let Some(field) = output.field() else {
                retained.push(output);
                continue;
            };
            let Some(metadata) = <R as OutputBindings>::FIELDS
                .iter()
                .find(|candidate| candidate.name == field)
            else {
                retained.push(output);
                continue;
            };
            if metadata.kind != super::outputs::OutputKind::State {
                if !bootstrap {
                    retained.push(output);
                }
                continue;
            }
            if bootstrap {
                if !metadata.bootstrap {
                    continue;
                }
            } else {
                let every = metadata.every_steps.unwrap_or(1);
                if every == 0 {
                    return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                        detail: format!("state output `{field}` has zero every_steps cadence"),
                    }));
                }
                let accepted_number = context
                    .invocation_index()
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!(ScheduleError::InvocationOverflow))?;
                if accepted_number % every != 0 {
                    continue;
                }
            }
            if metadata.on_change {
                let token = output.change_token().ok_or_else(|| {
                    anyhow::anyhow!(TransportError::InvalidMetadata {
                        detail: format!(
                            "state output `{field}` enabled on_change without a semantic token"
                        ),
                    })
                })?;
                if self
                    .last_state_values
                    .get(field)
                    .is_some_and(|previous| previous == token)
                {
                    continue;
                }
                self.last_state_values.insert(field, token.clone());
            }
            retained.push(output);
        }
        self.projections = retained;
        Ok(())
    }

    async fn bind(
        &mut self,
        bus: crate::bus::BusHandle,
        instance: &str,
        manifest: &RuntimeLaunchManifest,
    ) -> crate::Result<()>
    where
        R: RegisteredRuntime,
        R::Inputs: TransportInputSet,
        R::Outputs: OutputSet,
    {
        let session = bus.session()?;
        let mut read_subscriptions = Vec::new();
        for field in <R as OutputBindings>::FIELDS {
            if field.kind != super::outputs::OutputKind::Read {
                continue;
            }
            let signature = field.port_signature.ok_or_else(|| {
                anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("read output `{}` has no generated descriptor", field.name),
                })
            })?;
            let max_request_bytes = field.max_request_bytes.ok_or_else(|| {
                anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("read output `{}` has no request bound", field.name),
                })
            })?;
            if max_request_bytes == 0 {
                return Err(anyhow::anyhow!(TransportError::BatchTooLarge {
                    port: signature.name.to_owned(),
                    what: "request bytes",
                    actual: 0,
                    maximum: max_request_bytes,
                }));
            }
            let key = bus.full_key(&transport::port_key(instance, signature.name, "request"));
            let key_expr = zenoh::key_expr::OwnedKeyExpr::new(key.clone()).map_err(|error| {
                anyhow::anyhow!(TransportError::Transport(format!(
                    "invalid generated Runtime read key `{key}`: {error}"
                )))
            })?;
            let subscriber = session
                .declare_subscriber(key_expr)
                .with(zenoh::handlers::FifoChannel::new(2))
                .await
                .map_err(|error| anyhow::anyhow!(TransportError::Transport(error.to_string())))?;
            let allowed_callers = manifest
                .command_ranks(instance)?
                .into_iter()
                .filter_map(|((port, caller), rank)| {
                    (port == signature.name).then_some((caller, rank))
                })
                .collect();
            read_subscriptions.push(ReadSubscription {
                field: field.name,
                binding: super::transport::PortBinding::from_signature(signature),
                signature,
                max_request_bytes,
                allowed_callers,
                subscriber,
            });
        }

        let mut activation_routes = BTreeMap::new();
        for field in <R as OutputBindings>::FIELDS {
            if field.kind != super::outputs::OutputKind::Activate {
                continue;
            }
            let input_name = field.input.ok_or_else(|| {
                anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("activation output `{}` has no input selector", field.name),
                })
            })?;
            let input = <R::Inputs as TransportInputSet>::transport_fields()
                .iter()
                .find(|candidate| candidate.name == input_name)
                .ok_or_else(|| {
                    anyhow::anyhow!(TransportError::InvalidMetadata {
                        detail: format!(
                            "activation output `{}` selects unknown input `{input_name}`",
                            field.name
                        ),
                    })
                })?;
            if input.kind == super::input::InputKind::Operation {
                continue;
            }
            let routes = manifest.input_routes(input)?;
            if routes.len() != 1 {
                return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!(
                        "activation input `{input_name}` requires exactly one connected source"
                    ),
                }));
            }
            let Some(route) = routes.into_iter().next() else {
                return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!(
                        "activation input `{input_name}` requires exactly one connected source"
                    ),
                }));
            };
            if route.direction != InputDirection::Reply {
                return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!(
                        "activation input `{input_name}` is not a Read or Request completion"
                    ),
                }));
            }
            if route.request_max_bytes.is_none() {
                return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("activation input `{input_name}` has no request-byte bound"),
                }));
            }
            if route.caller_identity.is_none() || route.caller_rank.is_none() {
                return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!(
                        "activation input `{input_name}` has no graph-resolved caller ordinal"
                    ),
                }));
            }
            if activation_routes.insert(input_name, route).is_some() {
                return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("input `{input_name}` has multiple activation bindings"),
                }));
            }
        }
        self.bus = Some(bus);
        self.instance = Some(instance.to_owned());
        self.read_subscriptions = read_subscriptions;
        self.activation_routes = activation_routes;
        Ok(())
    }

    #[cfg(test)]
    fn bind_direct(&mut self, bus: crate::bus::BusHandle, instance: &str) {
        self.bus = Some(bus);
        self.instance = Some(instance.to_owned());
    }

    fn ensure_open(&self) -> crate::Result<()> {
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(crate::bus::BusError::Closed))?;
        if matches!(bus.terminal(), crate::bus::BusTerminal::Open) {
            Ok(())
        } else {
            Err(anyhow::anyhow!(crate::bus::BusError::Closed))
        }
    }

    fn next_command_id(&mut self) -> crate::Result<u64> {
        let id = self.next_command_id;
        self.next_command_id = self.next_command_id.checked_add(1).ok_or_else(|| {
            anyhow::anyhow!(TransportError::CommandCorrelation(
                "runtime command id space exhausted".to_owned(),
            ))
        })?;
        Ok(id)
    }

    fn queue_exchange_completion(&self, completion: ExchangeCompletion) -> crate::Result<()> {
        let queue = self.exchange_completions.as_ref().ok_or_else(|| {
            anyhow::anyhow!(TransportError::Transport(
                "exchange completion queue is not bound".to_owned(),
            ))
        })?;
        let mut queue = match queue.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        if queue.len() >= MAX_EXPIRED_CORRELATIONS {
            return Err(anyhow::anyhow!(TransportError::BatchTooLarge {
                port: "runtime".to_owned(),
                what: "exchange completion count",
                actual: queue.len() as u64 + 1,
                maximum: MAX_EXPIRED_CORRELATIONS as u64,
            }));
        }
        queue.push(completion);
        Ok(())
    }

    fn expire_correlations(&mut self) -> crate::Result<()> {
        let Some(correlations) = &self.correlations else {
            return Ok(());
        };
        let expired_set = self.expired_correlations.as_ref().ok_or_else(|| {
            anyhow::anyhow!(TransportError::Transport(
                "expired correlation table is not bound".to_owned(),
            ))
        })?;
        let completion_queue = self.exchange_completions.as_ref().ok_or_else(|| {
            anyhow::anyhow!(TransportError::Transport(
                "exchange completion queue is not bound".to_owned(),
            ))
        })?;
        let now = Instant::now();
        let retired = {
            let mut correlations = match correlations.lock() {
                Ok(correlations) => correlations,
                Err(poisoned) => poisoned.into_inner(),
            };
            let current = std::mem::take(&mut *correlations);
            let mut retained = BTreeMap::new();
            let mut retired = Vec::new();
            for (identity, pending) in current {
                if pending.deadline <= now {
                    retired.push((identity, pending));
                } else {
                    retained.insert(identity, pending);
                }
            }
            *correlations = retained;
            retired
        };
        if retired.is_empty() {
            return Ok(());
        }
        let mut expired_set = match expired_set.lock() {
            Ok(expired_set) => expired_set,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut completion_queue = match completion_queue.lock() {
            Ok(completion_queue) => completion_queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        for (identity, pending) in retired {
            if completion_queue.len() >= MAX_EXPIRED_CORRELATIONS {
                return Err(anyhow::anyhow!(TransportError::BatchTooLarge {
                    port: "runtime".to_owned(),
                    what: "exchange completion count",
                    actual: completion_queue.len() as u64 + 1,
                    maximum: MAX_EXPIRED_CORRELATIONS as u64,
                }));
            }
            expired_set.insert(identity.clone());
            while expired_set.len() > MAX_EXPIRED_CORRELATIONS {
                let _ = expired_set.pop_first();
            }
            match pending.kind {
                ExchangeKind::Read => completion_queue.push(ExchangeCompletion::Read {
                    field: pending.field,
                    key: pending.key,
                    result: Err(ReadError::Timeout),
                }),
                ExchangeKind::Request => completion_queue.push(ExchangeCompletion::Request {
                    field: pending.field,
                    key: pending.key,
                    result: Err(RequestError::OutcomeUnknown(
                        "request transfer deadline elapsed after admission".to_owned(),
                    )),
                }),
            }
        }
        Ok(())
    }

    fn poll_reads(&mut self) -> crate::Result<()> {
        for subscription_index in 0..self.read_subscriptions.len() {
            loop {
                let sample = match self.read_subscriptions[subscription_index]
                    .subscriber
                    .try_recv()
                {
                    Ok(Some(sample)) => WireSample::from_zenoh(sample)?,
                    Ok(None) => break,
                    Err(error) => {
                        return Err(anyhow::anyhow!(TransportError::Transport(
                            error.to_string(),
                        )));
                    }
                };
                let subscription = &self.read_subscriptions[subscription_index];
                let ingress = validate_read_request_metadata(
                    &subscription.binding,
                    &subscription.allowed_callers,
                    &sample,
                )?;
                if sample.payload().len() as u64 > subscription.max_request_bytes {
                    return Err(anyhow::anyhow!(TransportError::BodyTooLarge {
                        port: subscription.binding.name.clone(),
                        bytes: sample.payload().len(),
                        maximum: subscription.max_request_bytes,
                    }));
                }
                transport::validate_request(
                    subscription.signature,
                    &sample,
                    subscription.max_request_bytes,
                )?;
                if self
                    .pending_reads
                    .iter()
                    .any(|request| request.field == subscription.field)
                {
                    return Err(anyhow::anyhow!(TransportError::BatchTooLarge {
                        port: subscription.binding.name.clone(),
                        what: "outstanding read requests",
                        actual: 2,
                        maximum: 1,
                    }));
                }
                let field = subscription.field;
                if let transport::CommandIngress::External { ingress_sequence } = ingress
                    && self
                        .external_read_high_watermarks
                        .get(field)
                        .is_some_and(|previous| ingress_sequence <= *previous)
                {
                    return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                        format!(
                            "stale or replayed external Read ingress sequence {ingress_sequence}"
                        ),
                    )));
                }
                self.pending_reads
                    .push(RuntimeReadRequest { field, sample });
                if let transport::CommandIngress::External { ingress_sequence } = ingress {
                    self.external_read_high_watermarks
                        .insert(field, ingress_sequence);
                }
            }
        }
        Ok(())
    }

    fn poll_operations(&mut self) -> crate::Result<()> {
        let fields = self.operations.keys().copied().collect::<Vec<_>>();
        for field in fields {
            let Some(operation) = self.operations.get_mut(field) else {
                return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("operation `{field}` disappeared during polling"),
                }));
            };
            if let Some(completion) = operation
                .operation
                .poll()
                .map_err(|error| operation_error(field, error))?
            {
                let (id, outcome) = completion.into_parts();
                let key = operation.keys.remove(&id).ok_or_else(|| {
                    anyhow::anyhow!(TransportError::CommandCorrelation(format!(
                        "operation `{field}` completed with unknown internal id {id}"
                    )))
                })?;
                let result = match outcome {
                    OperationOutcome::Completed(Ok(value)) => Ok(value),
                    OperationOutcome::Completed(Err(error)) => {
                        Err(OperationInputError::Failed(error.to_string()))
                    }
                    OperationOutcome::TimedOut => Err(OperationInputError::Timeout),
                };
                let queue = self.operation_completions.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(TransportError::Transport(
                        "operation completion queue is not bound".to_owned(),
                    ))
                })?;
                let mut queue = match queue.lock() {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
                queue.push(OperationCompletionRecord { field, key, result });
            }
            if operation.operation.state() == super::operation::OperationState::Idle
                && operation.operation.has_pending()
            {
                operation
                    .operation
                    .start_pending()
                    .map_err(|error| operation_error(field, error))?;
                operation.pending_key = None;
            }
        }
        Ok(())
    }

    fn poll_transport(&mut self) -> crate::Result<()> {
        self.ensure_open()?;
        self.expire_correlations()?;
        self.poll_reads()?;
        self.poll_operations()
    }

    fn dispatch_activations(&mut self, activations: Vec<StagedActivation>) -> crate::Result<()> {
        for activation in activations {
            let field = activation.field;
            if let Some(completion) = activation.local_completion {
                self.queue_exchange_completion(completion)?;
                continue;
            }
            if let Some(worker) = activation.worker {
                let operation_completions =
                    self.operation_completions.clone().ok_or_else(|| {
                        anyhow::anyhow!(TransportError::Transport(
                            "operation completion queue is not bound".to_owned(),
                        ))
                    })?;
                let key = activation.key.ok_or_else(|| {
                    anyhow::anyhow!(TransportError::InvalidMetadata {
                        detail: format!("operation `{field}` activation has no key"),
                    })
                })?;
                let input = activation.request.ok_or_else(|| {
                    anyhow::anyhow!(TransportError::InvalidMetadata {
                        detail: format!("operation `{field}` activation has no input"),
                    })
                })?;
                let timeout_ms = activation.timeout_ms.ok_or_else(|| {
                    anyhow::anyhow!(TransportError::InvalidMetadata {
                        detail: format!("operation `{field}` activation has no timeout"),
                    })
                })?;
                let cancel_grace_ms = activation.cancel_grace_ms.ok_or_else(|| {
                    anyhow::anyhow!(TransportError::InvalidMetadata {
                        detail: format!("operation `{field}` activation has no cancel grace"),
                    })
                })?;
                if timeout_ms == 0 || cancel_grace_ms == 0 {
                    return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                        detail: format!("operation `{field}` has zero lifecycle bound"),
                    }));
                }
                let policy = OperationPolicy::from_millis(timeout_ms, cancel_grace_ms)
                    .map_err(|error| anyhow::anyhow!(error))?;
                let id = self.next_command_id()?;
                let operation =
                    self.operations
                        .entry(field)
                        .or_insert_with(|| ErasedManagedOperation {
                            operation: ManagedOperation::new(policy, worker),
                            keys: BTreeMap::new(),
                            pending_key: None,
                        });
                operation.keys.insert(id, key);
                match operation
                    .operation
                    .submit(super::Activation::new(id, input))
                    .map_err(|error| operation_error(field, error))?
                {
                    super::operation::SubmitResult::Started => {}
                    super::operation::SubmitResult::Pending => {
                        operation.pending_key = Some(id);
                    }
                    super::operation::SubmitResult::ReplacedPending => {
                        if let Some(previous) = operation.pending_key.replace(id) {
                            let previous_key = operation.keys.remove(&previous).ok_or_else(|| {
                                anyhow::anyhow!(TransportError::CommandCorrelation(format!(
                                    "operation `{field}` replaced unknown pending activation {previous}"
                                )))
                            })?;
                            let mut queue = match operation_completions.lock() {
                                Ok(queue) => queue,
                                Err(poisoned) => poisoned.into_inner(),
                            };
                            if queue.len() >= MAX_EXPIRED_CORRELATIONS {
                                return Err(anyhow::anyhow!(TransportError::BatchTooLarge {
                                    port: "runtime".to_owned(),
                                    what: "operation completion count",
                                    actual: queue.len() as u64 + 1,
                                    maximum: MAX_EXPIRED_CORRELATIONS as u64,
                                }));
                            }
                            queue.push(OperationCompletionRecord {
                                field,
                                key: previous_key,
                                result: Err(OperationInputError::Stale),
                            });
                        }
                    }
                }
                continue;
            }

            let command_id = activation.command_id.ok_or_else(|| {
                anyhow::anyhow!(TransportError::CommandCorrelation(format!(
                    "remote activation `{field}` has no command id"
                )))
            })?;
            let kind = activation.correlation_kind.ok_or_else(|| {
                anyhow::anyhow!(TransportError::CommandCorrelation(format!(
                    "remote activation `{field}` has no exchange kind"
                )))
            })?;
            let timeout_ms = activation.timeout_ms.ok_or_else(|| {
                anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("remote activation `{field}` has no timeout"),
                })
            })?;
            let key = activation.key.ok_or_else(|| {
                anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("remote activation `{field}` has no key"),
                })
            })?;
            let expected_source = activation.expected_source.ok_or_else(|| {
                anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("remote activation `{field}` has no target source"),
                })
            })?;
            let correlations = self.correlations.as_ref().ok_or_else(|| {
                anyhow::anyhow!(TransportError::Transport(
                    "activation correlation table is not bound".to_owned(),
                ))
            })?;
            let deadline = Instant::now()
                .checked_add(Duration::from_millis(timeout_ms))
                .ok_or_else(|| {
                    anyhow::anyhow!(TransportError::CommandCorrelation(
                        "runtime transfer deadline overflowed".to_owned(),
                    ))
                })?;
            let mut correlations = match correlations.lock() {
                Ok(correlations) => correlations,
                Err(poisoned) => poisoned.into_inner(),
            };
            if correlations.keys().any(|(candidate, _)| candidate == field) {
                return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                    format!("activation `{field}` acquired a duplicate outstanding correlation")
                )));
            }
            correlations.insert(
                (field.to_owned(), command_id),
                PendingCorrelation {
                    field,
                    key,
                    kind,
                    deadline,
                    expected_source,
                },
            );
            drop(correlations);
            if let (Some(every), Some(invocation_index)) =
                (activation.refresh_every_steps, activation.invocation_index)
            {
                self.next_refresh_steps
                    .insert(field, invocation_index.saturating_add(every));
            }
        }
        Ok(())
    }
}

impl<R> RuntimeWorkSink for ExecutionOutputAdapter<R>
where
    R: RegisteredRuntime,
    R::Inputs: InputSet + TransportInputSet,
    R::Outputs: OutputSet,
{
    fn activate(
        &mut self,
        field: &'static str,
        key: TransportValue,
        request: TransportValue,
        worker: Option<OperationWorker>,
        timeout_ms: Option<u64>,
        refresh_every_steps: Option<u64>,
        cancel_grace_ms: Option<u64>,
        context: super::StepContext,
    ) -> crate::Result<()> {
        if self.stopped {
            return Err(anyhow::anyhow!(crate::bus::BusError::Closed));
        }
        if self
            .staged
            .iter()
            .any(|activation| activation.field == field)
        {
            return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                format!("input `{field}` has more than one activation in one candidate")
            )));
        }
        if let Some(worker) = worker {
            if timeout_ms.is_none() || cancel_grace_ms.is_none() || refresh_every_steps.is_some() {
                return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("local operation `{field}` has invalid activation policy"),
                }));
            }
            self.staged.push(StagedActivation {
                field,
                key: Some(key),
                request: Some(request),
                worker: Some(worker),
                request_output: None,
                timeout_ms,
                refresh_every_steps: None,
                invocation_index: None,
                cancel_grace_ms,
                command_id: None,
                correlation_kind: None,
                expected_source: None,
                local_completion: None,
            });
            return Ok(());
        }

        let route = self.activation_routes.get(field).cloned().ok_or_else(|| {
            anyhow::anyhow!(TransportError::InvalidMetadata {
                detail: format!("activation input `{field}` has no graph-resolved route"),
            })
        })?;
        let timeout_ms = timeout_ms.ok_or_else(|| {
            anyhow::anyhow!(TransportError::InvalidMetadata {
                detail: format!("remote activation `{field}` has no timeout"),
            })
        })?;
        if timeout_ms == 0
            || (refresh_every_steps.is_some() && route.binding.kind != crate::port::PortKind::Read)
        {
            return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                detail: format!("remote activation `{field}` has invalid timeout/refresh policy"),
            }));
        }
        if cancel_grace_ms.is_some() {
            return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                detail: format!("remote activation `{field}` has local operation retirement data"),
            }));
        }
        if let Some(every) = refresh_every_steps {
            let current = context.invocation_index();
            if self
                .next_refresh_steps
                .get(field)
                .is_some_and(|next| current < *next)
            {
                // Refresh opportunities are counted from accepted invocation
                // boundaries.  A healthy paused/busy residence consumes no
                // transfer timeout and does not create catch-up requests.
                return Ok(());
            }
            if every == 0 {
                return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("remote Read activation `{field}` has zero refresh period"),
                }));
            }
        }
        let kind = if route.binding.kind == crate::port::PortKind::Read {
            ExchangeKind::Read
        } else {
            ExchangeKind::Request
        };
        let max_bytes = route.request_max_bytes.ok_or_else(|| {
            anyhow::anyhow!(TransportError::InvalidMetadata {
                detail: format!("remote activation `{field}` has no request-byte bound"),
            })
        })?;
        let command_id = self.next_command_id()?;
        let request_payload =
            match <R::Inputs as TransportInputSet>::encode_request(field, &*request) {
                Ok(payload) => payload,
                Err(error) => {
                    self.staged.push(StagedActivation {
                        field,
                        key: None,
                        request: None,
                        worker: None,
                        request_output: None,
                        timeout_ms: Some(timeout_ms),
                        refresh_every_steps,
                        invocation_index: Some(context.invocation_index()),
                        cancel_grace_ms: None,
                        command_id: None,
                        correlation_kind: None,
                        expected_source: None,
                        local_completion: Some(not_sent_completion(
                            field,
                            key,
                            kind,
                            error.to_string(),
                        )),
                    });
                    return Ok(());
                }
            };
        let correlations = self.correlations.as_ref().ok_or_else(|| {
            anyhow::anyhow!(TransportError::Transport(
                "activation correlation table is not bound".to_owned(),
            ))
        })?;
        {
            let correlations = match correlations.lock() {
                Ok(correlations) => correlations,
                Err(poisoned) => poisoned.into_inner(),
            };
            if correlations.keys().any(|(candidate, _)| candidate == field) {
                if refresh_every_steps.is_some() {
                    return Ok(());
                }
                return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                    format!("activation `{field}` already has an outstanding completion")
                )));
            }
            let source = self.instance.as_deref().ok_or_else(|| {
                anyhow::anyhow!(TransportError::Transport(
                    "activation transport is not bound".to_owned(),
                ))
            })?;
            let caller_identity = route.caller_identity.as_deref().ok_or_else(|| {
                anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("remote activation `{field}` has no graph caller identity"),
                })
            })?;
            let caller_rank = route.caller_rank.ok_or_else(|| {
                anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("remote activation `{field}` has no graph caller rank"),
                })
            })?;
            let metadata = transport::request_metadata(
                source,
                caller_identity,
                context,
                command_id,
                context.invocation_index().saturating_add(1),
                caller_rank,
            );
            let output = match PreparedOutput::request_binding(
                route.binding.clone(),
                request_payload,
                max_bytes,
                metadata,
            ) {
                Ok(output) => output
                    .for_field(field)
                    .for_instance(route.source_instance.clone()),
                Err(error) => {
                    self.staged.push(StagedActivation {
                        field,
                        key: None,
                        request: None,
                        worker: None,
                        request_output: None,
                        timeout_ms: Some(timeout_ms),
                        refresh_every_steps,
                        invocation_index: Some(context.invocation_index()),
                        cancel_grace_ms: None,
                        command_id: None,
                        correlation_kind: None,
                        expected_source: None,
                        local_completion: Some(not_sent_completion(
                            field,
                            key,
                            kind,
                            error.to_string(),
                        )),
                    });
                    return Ok(());
                }
            };
            self.staged.push(StagedActivation {
                field,
                key: Some(key),
                request: None,
                worker: None,
                request_output: Some(output),
                timeout_ms: Some(timeout_ms),
                refresh_every_steps,
                invocation_index: Some(context.invocation_index()),
                cancel_grace_ms: None,
                command_id: Some(command_id),
                correlation_kind: Some(kind),
                expected_source: Some(route.source_instance),
                local_completion: None,
            });
        }
        Ok(())
    }

    fn push_read_reply(&mut self, output: PreparedOutput) -> crate::Result<()> {
        self.projections.push(output);
        Ok(())
    }
}

impl<R> OutputAdmission<R::Outputs> for ExecutionOutputAdapter<R>
where
    R: RegisteredRuntime,
    R::Inputs: InputSet + TransportInputSet,
    R::Outputs: OutputSet,
{
    type Reservation = OutputReservation;

    fn reserve(&mut self, outputs: &R::Outputs) -> crate::Result<Self::Reservation> {
        if self.stopped {
            return Err(anyhow::anyhow!(crate::bus::BusError::Closed));
        }
        self.ensure_open()?;
        let context = self.context.take().ok_or_else(|| {
            anyhow::anyhow!(TransportError::InvalidMetadata {
                detail: "output reservation has no invocation context".to_owned(),
            })
        })?;
        let resolve_input_port = |field: &str| transport::input_port_signature::<R::Inputs>(field);
        let transient = outputs.encode_transport(
            context,
            &resolve_input_port,
            self.instance.as_deref().unwrap_or_default(),
        )?;
        let mut records = std::mem::take(&mut self.projections);
        records.extend(transient);
        let mut activations = std::mem::take(&mut self.staged);
        for activation in &mut activations {
            if let Some(request) = activation.request_output.take() {
                records.push(request);
            }
        }
        Ok(OutputReservation {
            records,
            activations,
        })
    }
}

impl<R> OutputSink<R> for ExecutionOutputAdapter<R>
where
    R: RegisteredRuntime,
    R::Inputs: InputSet + TransportInputSet,
    R::Outputs: OutputSet,
{
    fn prepare(&mut self, context: &super::StepContext) -> crate::Result<()> {
        self.ensure_open()?;
        self.context = Some(*context);
        Ok(())
    }

    fn prepare_delivery(&mut self, boundary: u64, timeline_id: &str) -> crate::Result<()> {
        self.ensure_open()?;
        if timeline_id.is_empty() {
            return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                detail: "controlled delivery timeline cannot be empty".to_owned(),
            }));
        }
        self.delivery_context = Some((boundary, timeline_id.to_owned()));
        Ok(())
    }

    fn prepare_state(
        &mut self,
        service: &R,
        state: &R::State,
        context: &super::StepContext,
    ) -> crate::Result<()> {
        self.ensure_open()?;
        self.projections.clear();
        let source = self.instance.as_deref().unwrap_or_default().to_owned();
        let mut requests = std::mem::take(&mut self.pending_reads);
        service.serve_reads(state, *context, &mut requests, &source, self)?;
        if !requests.is_empty() {
            return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
                detail: format!(
                    "{} read request(s) had no generated handler in the selected runtime",
                    requests.len()
                ),
            }));
        }
        service.prepare_work(state, *context, self)?;
        let resolve_input_port = |field: &str| transport::input_port_signature::<R::Inputs>(field);
        self.projections.extend(service.encode_transport(
            state,
            *context,
            &resolve_input_port,
            &source,
        )?);
        self.filter_state_projections(*context, false)?;
        Ok(())
    }

    fn bootstrap(
        &mut self,
        service: &R,
        state: &R::State,
        now: ExecutionTime,
    ) -> crate::Result<()> {
        // Transport-free direct runners have no publication side effect.
        if self.bus.is_none() {
            return Ok(());
        }
        self.ensure_open()?;
        self.projections.clear();
        let context = super::StepContext::first(now, R::SPEC.period);
        let source = self.instance.as_deref().unwrap_or_default().to_owned();
        let resolve_input_port = |field: &str| transport::input_port_signature::<R::Inputs>(field);
        self.projections.extend(service.encode_transport(
            state,
            context,
            &resolve_input_port,
            &source,
        )?);
        self.filter_state_projections(context, true)?;
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(crate::bus::BusError::Closed))?;
        transport::publish_batch(bus, &source, &self.projections)?;
        self.projections.clear();
        Ok(())
    }

    fn poll(&mut self) -> crate::Result<()> {
        self.poll_transport()
    }

    fn publish(
        &mut self,
        accepted: AcceptedInvocation<R::Outputs, Self::Reservation>,
    ) -> crate::Result<()> {
        self.ensure_open()?;
        let (_invocation, _context, _outputs, reservation) = accepted.into_parts();
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(crate::bus::BusError::Closed))?;
        let mut records = reservation.records;
        self.last_delivery_receipts.clear();
        if let Some((boundary, timeline_id)) = self.delivery_context.take() {
            let execution_id = bus.execution().to_string();
            let mut item_indices = BTreeMap::<(String, String, Option<String>), u32>::new();
            for record in &mut records {
                let key = record.delivery_route();
                let item = item_indices.entry(key).or_insert(0);
                let item_index = *item;
                *item = item_index.saturating_add(1);
                record.stamp_delivery_identity(&execution_id, &timeline_id, boundary, item_index);
                if let Some((port, direction, target, sequence, item, bytes)) =
                    record.delivery_receipt(item_index)
                {
                    self.last_delivery_receipts.push(RuntimeDeliveryReceipt {
                        port,
                        direction,
                        target,
                        sequence,
                        item,
                        bytes,
                    });
                }
            }
        }
        transport::publish_batch(bus, self.instance.as_deref().unwrap_or_default(), &records)?;
        let mut receipts = BTreeMap::<String, RuntimeProductReceipt>::new();
        for record in &records {
            let Some((port, sequence, bytes)) = record.product_receipt() else {
                continue;
            };
            let entry = receipts
                .entry(port.clone())
                .or_insert(RuntimeProductReceipt {
                    port,
                    sequence,
                    items: 0,
                    bytes: 0,
                });
            entry.sequence = sequence;
            entry.items = entry.items.saturating_add(1);
            entry.bytes = entry.bytes.saturating_add(bytes);
        }
        self.last_product_receipts = receipts.into_values().collect();
        self.last_actuations = records
            .iter()
            .filter_map(PreparedOutput::actuation)
            .map(|(port, payload, valid_until_ns)| RuntimeActuation {
                port,
                payload,
                valid_until_ns,
            })
            .collect();
        self.dispatch_activations(reservation.activations)
    }

    fn take_product_receipts(&mut self) -> Vec<RuntimeProductReceipt> {
        std::mem::take(&mut self.last_product_receipts)
    }

    fn take_delivery_receipts(&mut self) -> Vec<RuntimeDeliveryReceipt> {
        std::mem::take(&mut self.last_delivery_receipts)
    }

    fn take_actuations(&mut self) -> Vec<RuntimeActuation> {
        std::mem::take(&mut self.last_actuations)
    }

    fn stop(&mut self) -> crate::Result<()> {
        self.stopped = true;
        self.context = None;
        self.projections.clear();
        self.pending_reads.clear();
        self.staged.clear();
        self.next_refresh_steps.clear();
        self.last_state_values.clear();
        self.last_product_receipts.clear();
        self.last_delivery_receipts.clear();
        self.last_actuations.clear();
        self.delivery_context = None;
        self.external_read_high_watermarks.clear();
        self.read_subscriptions.clear();
        let mut first_error = None;
        for operation in self.operations.values_mut() {
            if let Err(error) = operation.operation.reset() {
                first_error.get_or_insert(anyhow::anyhow!(error));
            }
            operation.keys.clear();
            operation.pending_key = None;
        }
        self.operations.clear();
        if let Some(correlations) = &self.correlations {
            match correlations.lock() {
                Ok(mut correlations) => correlations.clear(),
                Err(poisoned) => poisoned.into_inner().clear(),
            }
        }
        if let Some(expired) = &self.expired_correlations {
            match expired.lock() {
                Ok(mut expired) => expired.clear(),
                Err(poisoned) => poisoned.into_inner().clear(),
            }
        }
        if let Some(queue) = &self.operation_completions {
            match queue.lock() {
                Ok(mut queue) => queue.clear(),
                Err(poisoned) => poisoned.into_inner().clear(),
            }
        }
        if let Some(queue) = &self.exchange_completions {
            match queue.lock() {
                Ok(mut queue) => queue.clear(),
                Err(poisoned) => poisoned.into_inner().clear(),
            }
        }
        self.bus = None;
        self.instance = None;
        first_error.map_or(Ok(()), Err)
    }

    fn reset(&mut self) -> crate::Result<()> {
        self.stopped = false;
        self.context = None;
        self.projections.clear();
        self.pending_reads.clear();
        self.staged.clear();
        self.next_refresh_steps.clear();
        self.last_state_values.clear();
        self.external_read_high_watermarks.clear();
        self.next_command_id = 1;
        self.last_product_receipts.clear();
        self.last_actuations.clear();
        for operation in self.operations.values_mut() {
            operation
                .operation
                .reset()
                .map_err(|error| anyhow::anyhow!(error))?;
            operation.keys.clear();
            operation.pending_key = None;
        }
        if let Some(correlations) = &self.correlations {
            match correlations.lock() {
                Ok(mut correlations) => correlations.clear(),
                Err(poisoned) => poisoned.into_inner().clear(),
            }
        }
        if let Some(expired) = &self.expired_correlations {
            match expired.lock() {
                Ok(mut expired) => expired.clear(),
                Err(poisoned) => poisoned.into_inner().clear(),
            }
        }
        if let Some(queue) = &self.operation_completions {
            match queue.lock() {
                Ok(mut queue) => queue.clear(),
                Err(poisoned) => poisoned.into_inner().clear(),
            }
        }
        if let Some(queue) = &self.exchange_completions {
            match queue.lock() {
                Ok(mut queue) => queue.clear(),
                Err(poisoned) => poisoned.into_inner().clear(),
            }
        }
        if self.bus.is_none() || self.instance.is_none() {
            return Err(anyhow::anyhow!(TransportError::Transport(
                "Runtime output transport is not bound after reset".to_owned(),
            )));
        }
        Ok(())
    }
}

/// Result of one bounded runner poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollOutcome {
    /// The release was not due yet.
    NotDue { next_release: ExecutionTime },
    /// One complete invocation was accepted and published.
    Accepted { invocation_index: u64 },
    /// Stop was requested before selecting a candidate.
    Stopped,
}

/// Result of one supervisor-controlled invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ControlledInvocationOutcome {
    /// Boundary selected by the supervisor.
    pub(crate) boundary: u64,
    /// Zero-based invocation index owned by this runtime.
    pub(crate) invocation_index: u64,
    /// Products published after complete local acceptance.
    pub(crate) required_products: Vec<RuntimeProductReceipt>,
    /// Exact output records whose graph receivers must acknowledge admission.
    pub(crate) required_deliveries: Vec<RuntimeDeliveryReceipt>,
    /// Input records frozen before this invocation was accepted.
    pub(crate) required_inputs: Vec<RuntimeInputReceipt>,
    /// Typed actuator-facing setpoints published by this invocation.
    pub(crate) actuations: Vec<RuntimeActuation>,
}

/// A complete hardware runtime owner and its transport adapters.
pub struct RuntimeRunner<R, Inputs, Outputs>
where
    R: RegisteredRuntime,
    Inputs: InputSource<R>,
    Outputs: OutputSink<R>,
{
    owner: RuntimeOwner<R>,
    schedule: HardwareSchedule,
    inputs: Inputs,
    outputs: Outputs,
    stopped: bool,
    controlled_previous: Option<ExecutionTime>,
}

impl<R, Inputs, Outputs> RuntimeRunner<R, Inputs, Outputs>
where
    R: RegisteredRuntime,
    R::Inputs: InputSnapshot,
    Inputs: InputSource<R>,
    Outputs: OutputSink<R>,
{
    /// Initialize one owner, validate its specification, and bind adapters.
    pub fn new(
        service: R,
        now: ExecutionTime,
        config: R::Config,
        inputs: Inputs,
        outputs: Outputs,
    ) -> crate::Result<Self> {
        let initialization_started = Instant::now();
        let owner = RuntimeOwner::new(service, now, config)?;
        if initialization_started.elapsed() > R::SPEC.init_timeout.as_duration() {
            return Err(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        let schedule = HardwareSchedule::new(now, R::SPEC.period)
            .map_err(|error| anyhow::anyhow!(RunnerError::Schedule(error)))?;
        let mut outputs = outputs;
        if let Some(state) = owner.state_ref() {
            outputs.bootstrap(owner.service(), state, now)?;
        }
        Ok(Self {
            owner,
            schedule,
            inputs,
            outputs,
            stopped: false,
            controlled_previous: None,
        })
    }

    /// Poll one candidate at an explicit host input-freeze time.
    pub fn poll(&mut self, now: ExecutionTime) -> crate::Result<PollOutcome> {
        if self.stopped {
            return Ok(PollOutcome::Stopped);
        }
        if let Err(error) = catch_adapter(|| self.outputs.poll()) {
            return self.fail(error);
        }
        let candidate = match self.schedule.candidate(now) {
            Ok(candidate) => candidate,
            Err(ScheduleError::NotDue { next_release }) => {
                return Ok(PollOutcome::NotDue { next_release });
            }
            Err(error) => return self.fail(anyhow::anyhow!(RunnerError::Schedule(error))),
        };
        let started = Instant::now();
        let inputs = match catch_adapter(|| self.inputs.freeze(&candidate)) {
            Ok(inputs) => inputs,
            Err(error) => return self.fail(error),
        };
        if started.elapsed() > R::SPEC.timeout.as_duration() {
            return self.fail(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        if let Err(error) = catch_adapter(|| self.outputs.prepare(&candidate.context())) {
            return self.fail(error);
        }
        let accepted = {
            let owner = &mut self.owner;
            let outputs = &mut self.outputs;
            match owner.accept_with_hook(
                &candidate.context(),
                &inputs,
                outputs,
                |service, state, context, outputs| outputs.prepare_state(service, state, context),
            ) {
                Ok(accepted) => accepted,
                Err(error) => return self.fail(error),
            }
        };
        if started.elapsed() > R::SPEC.timeout.as_duration() {
            return self.fail(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        let invocation_index = accepted.invocation().index();
        if let Err(error) = catch_adapter(|| self.outputs.publish(accepted)) {
            return self.fail(error);
        }
        if started.elapsed() > R::SPEC.timeout.as_duration() {
            return self.fail(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        if let Err(error) = self.schedule.accept(candidate) {
            return self.fail(anyhow::anyhow!(RunnerError::Schedule(error)));
        }
        Ok(PollOutcome::Accepted { invocation_index })
    }

    /// Execute one supervisor-controlled boundary.
    ///
    /// This path deliberately does not consult or advance [`HardwareSchedule`].
    /// A simulation supervisor owns the boundary sequence, while this runner
    /// retains the same single-freeze, single-invocation, reserve, and publish
    /// ordering as hardware execution.
    pub(crate) fn invoke_controlled(
        &mut self,
        boundary: u64,
        now: ExecutionTime,
        timeline_id: &str,
    ) -> crate::Result<ControlledInvocationOutcome> {
        if self.stopped {
            return Err(anyhow::anyhow!(crate::bus::BusError::Closed));
        }
        let context = StepContext::from_previous(
            now,
            R::SPEC.period,
            self.controlled_previous,
            0,
            self.owner.next_invocation().index(),
        );
        let candidate = HardwareInvocation::controlled(context);
        let started = Instant::now();
        if let Err(error) = catch_adapter(|| self.outputs.poll()) {
            return self.fail_controlled(error);
        }
        let inputs = match catch_adapter(|| self.inputs.freeze(&candidate)) {
            Ok(inputs) => inputs,
            Err(error) => return self.fail_controlled(error),
        };
        if started.elapsed() > R::SPEC.timeout.as_duration() {
            return self.fail_controlled(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        if let Err(error) = catch_adapter(|| self.outputs.prepare(&candidate.context())) {
            return self.fail_controlled(error);
        }
        let accepted = {
            let owner = &mut self.owner;
            let outputs = &mut self.outputs;
            match owner.accept_with_hook(
                &candidate.context(),
                &inputs,
                outputs,
                |service, state, context, outputs| outputs.prepare_state(service, state, context),
            ) {
                Ok(accepted) => accepted,
                Err(error) => return self.fail_controlled(error),
            }
        };
        if started.elapsed() > R::SPEC.timeout.as_duration() {
            return self.fail_controlled(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        let invocation_index = accepted.invocation().index();
        if let Err(error) = catch_adapter(|| self.outputs.prepare_delivery(boundary, timeline_id)) {
            return self.fail_controlled(error);
        }
        if let Err(error) = catch_adapter(|| self.outputs.publish(accepted)) {
            return self.fail_controlled(error);
        }
        if started.elapsed() > R::SPEC.timeout.as_duration() {
            return self.fail_controlled(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        let required_products = self.outputs.take_product_receipts();
        let required_deliveries = self.outputs.take_delivery_receipts();
        let required_inputs = self.inputs.take_input_receipts();
        let actuations = self.outputs.take_actuations();
        self.controlled_previous = Some(now);
        Ok(ControlledInvocationOutcome {
            boundary,
            invocation_index,
            required_products,
            required_deliveries,
            required_inputs,
            actuations,
        })
    }

    /// Fence receiver queues to a newly admitted controlled timeline.
    pub(crate) fn set_controlled_timeline(&mut self, timeline_id: &str) -> crate::Result<()> {
        self.inputs.set_timeline(timeline_id)
    }

    /// Drive the process until a host stop or a terminal lifecycle error.
    pub fn run_until_stop<C: RuntimeClock>(&mut self, clock: &mut C) -> crate::Result<()> {
        while !self.stopped {
            match self.poll(clock.now())? {
                PollOutcome::NotDue { next_release } => clock.wait_until(next_release)?,
                PollOutcome::Accepted { .. } => {}
                PollOutcome::Stopped => break,
            }
        }
        Ok(())
    }

    /// Request normal stop and clean up all owned transport/operation work.
    pub fn stop(&mut self) -> crate::Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        let input_result = catch_adapter(|| self.inputs.stop());
        let output_result = catch_adapter(|| self.outputs.stop());
        let result = input_result.and(output_result);
        if result.is_err() {
            self.owner.fail();
        }
        result
    }

    /// Reinitialize a fresh execution with an owned, possibly non-Clone config.
    pub fn reset(&mut self, now: ExecutionTime, config: R::Config) -> crate::Result<()> {
        let input_result = catch_adapter(|| self.inputs.reset());
        let output_result = catch_adapter(|| self.outputs.reset());
        if let Err(error) = input_result.and(output_result) {
            self.owner.fail();
            self.stopped = true;
            self.cleanup_after_failure();
            return Err(error);
        }
        if let Err(error) = self.owner.reset(now, config) {
            self.stopped = true;
            self.cleanup_after_failure();
            return Err(error);
        }
        if let Some(state) = self.owner.state_ref()
            && let Err(error) = self.outputs.bootstrap(self.owner.service(), state, now)
        {
            self.owner.fail();
            self.stopped = true;
            self.cleanup_after_failure();
            return Err(error);
        }
        self.schedule = HardwareSchedule::new(now, R::SPEC.period)
            .map_err(|error| anyhow::anyhow!(RunnerError::Schedule(error)))
            .inspect_err(|_error| {
                self.owner.fail();
                self.stopped = true;
                self.cleanup_after_failure();
            })?;
        self.controlled_previous = None;
        self.stopped = false;
        Ok(())
    }

    /// Runtime lifecycle status.
    #[must_use]
    pub const fn status(&self) -> RuntimeStatus {
        self.owner.status()
    }

    /// Current next nominal release.
    #[must_use]
    pub const fn next_release(&self) -> ExecutionTime {
        self.schedule.next_release()
    }

    fn fail(&mut self, error: anyhow::Error) -> crate::Result<PollOutcome> {
        self.owner.fail();
        self.stopped = true;
        self.cleanup_after_failure();
        Err(error)
    }

    fn fail_controlled(
        &mut self,
        error: anyhow::Error,
    ) -> crate::Result<ControlledInvocationOutcome> {
        self.owner.fail();
        self.stopped = true;
        self.cleanup_after_failure();
        Err(error)
    }

    fn cleanup_after_failure(&mut self) {
        let _ = catch_adapter(|| self.inputs.stop());
        let _ = catch_adapter(|| self.outputs.stop());
    }
}

fn catch_adapter<T>(operation: impl FnOnce() -> crate::Result<T>) -> crate::Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(super::InvocationError::Panicked)),
    }
}

/// Errors at the process/bundle runner boundary.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// A bundle filesystem operation failed.
    #[error("bundle I/O failed for {path}: {source}")]
    BundleIo {
        /// Affected path.
        path: PathBuf,
        /// Filesystem source error.
        #[source]
        source: std::io::Error,
    },
    /// The bundle manifest was not admissible.
    #[error("invalid runtime bundle: {message}")]
    BundleInvalid {
        /// Diagnostic detail.
        message: String,
    },
    /// The bundle manifest could not be decoded.
    #[error("cannot decode runtime bundle manifest {path}: {source}")]
    BundleJson {
        /// Manifest path.
        path: PathBuf,
        /// JSON source error.
        #[source]
        source: serde_json::Error,
    },
    /// The selected instance did not occur in the executable table.
    #[error("runtime instance `{instance}` is not admitted by the bundle")]
    UnknownInstance {
        /// Requested instance.
        instance: String,
    },
    /// A selected executable digest or size did not match the bundle record.
    #[error("runtime executable `{instance}` does not match its bundle digest")]
    ExecutableMismatch {
        /// Selected instance.
        instance: String,
    },
    /// A schedule candidate failed before acceptance.
    #[error("runtime schedule failed: {0}")]
    Schedule(#[from] ScheduleError),
    /// Generated contract bindings are required for process transport.
    #[error(
        "runtime instance `{instance}` cannot start on `{connect}` because generated typed input/output bindings are missing"
    )]
    TypedBindingsUnavailable {
        /// Selected runtime instance.
        instance: String,
        /// Explicit supervisor endpoint.
        connect: String,
    },
    /// A local operation outlived its cancellation grace.  The runtime has
    /// stopped admitting work and the supervisor must terminate this process
    /// before it can replace the operation owner.
    #[error("operation `{field}` requires supervisor process termination: {detail}")]
    ProcessTerminationRequired {
        /// Generated operation/input field.
        field: &'static str,
        /// Operation lifecycle detail.
        detail: String,
    },
}

fn operation_error(field: &'static str, error: super::operation::OperationError) -> anyhow::Error {
    match error {
        super::operation::OperationError::ProcessTerminationRequired => {
            anyhow::anyhow!(RunnerError::ProcessTerminationRequired {
                field,
                detail: "operation worker did not exit within cancel grace".to_owned(),
            })
        }
        error => anyhow::anyhow!(error),
    }
}

fn enforce_process_boundary(result: crate::Result<()>) -> crate::Result<()> {
    if requires_process_termination(&result) {
        terminate_process_boundary();
    }
    result
}

fn requires_process_termination(result: &crate::Result<()>) -> bool {
    result.as_ref().is_err_and(|error| {
        error.chain().any(|cause| {
            cause.downcast_ref::<RunnerError>().is_some_and(|error| {
                matches!(error, RunnerError::ProcessTerminationRequired { .. })
            })
        })
    })
}

#[cold]
fn terminate_process_boundary() -> ! {
    std::process::abort()
}

#[cfg(test)]
fn enforce_process_boundary_with(
    result: crate::Result<()>,
    terminate: impl FnOnce(),
) -> crate::Result<()> {
    if requires_process_termination(&result) {
        terminate();
    }
    result
}

#[derive(Debug, Deserialize)]
struct SourceBundleManifest {
    schema: String,
    robot_id: String,
    document: SourceDocument,
    executables: Vec<SourceExecutable>,
}

#[derive(Debug, Deserialize, Default)]
struct SourceDocument {
    #[serde(default)]
    services: BTreeMap<String, SourceService>,
    #[serde(default)]
    connections: BTreeMap<String, SourceConnectionSources>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SourceConnectionSources {
    One(String),
    Many(Vec<String>),
}

impl SourceConnectionSources {
    fn as_slice(&self) -> &[String] {
        match self {
            Self::One(source) => std::slice::from_ref(source),
            Self::Many(sources) => sources,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct SourceService {
    #[serde(default)]
    config: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct SourceExecutable {
    instance: String,
    path: String,
    bytes: u64,
    sha256: String,
    #[serde(default)]
    artifact: Option<SourceArtifact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct SourceArtifact {
    runtime: SourceRuntimeRecord,
}

#[derive(Clone, Debug, Deserialize, Default, Eq, PartialEq)]
struct SourceRuntimeRecord {
    #[serde(default)]
    period_ms: Option<u64>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    init_timeout_ms: Option<u64>,
    #[serde(default)]
    inputs: Vec<SourceInputRecord>,
    #[serde(default)]
    transient_outputs: Vec<SourceOutputRecord>,
    #[serde(default)]
    service_outputs: Vec<SourceOutputRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct SourceInputRecord {
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    max_items: Option<u64>,
    #[serde(default)]
    max_bytes: Option<u64>,
    #[serde(default)]
    port: Option<String>,
    #[serde(default)]
    signature: Option<SourcePortSignature>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct SourceOutputRecord {
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    port: Option<String>,
    #[serde(default)]
    signature: Option<SourcePortSignature>,
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    max_items: Option<u64>,
    #[serde(default)]
    max_bytes: Option<u64>,
    #[serde(default)]
    max_request_bytes: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct SourcePortSignature {
    name: String,
    service: String,
    method: String,
    kind: String,
    request: String,
    response: String,
}

impl SourcePortSignature {
    fn to_binding(&self) -> crate::Result<super::transport::PortBinding> {
        let kind = match self.kind.as_str() {
            "state" => crate::port::PortKind::State,
            "sample" => crate::port::PortKind::Sample,
            "event" => crate::port::PortKind::Event,
            "stream" => crate::port::PortKind::Stream,
            "setpoint" => crate::port::PortKind::Setpoint,
            "read" => crate::port::PortKind::Read,
            "commands" => crate::port::PortKind::Commands,
            value => {
                return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                    message: format!("unknown source port kind `{value}`"),
                }));
            }
        };
        Ok(super::transport::PortBinding {
            name: self.name.clone(),
            service: self.service.clone(),
            method: self.method.clone(),
            kind,
            request: self.request.clone(),
            response: self.response.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputDirection {
    Request,
    Publication,
    Reply,
    Completion,
}

impl InputDirection {
    fn for_kind(kind: super::input::InputKind) -> crate::Result<Self> {
        match kind {
            super::input::InputKind::Latest
            | super::input::InputKind::Samples
            | super::input::InputKind::Events
            | super::input::InputKind::Setpoint
            | super::input::InputKind::Stream => Ok(Self::Publication),
            super::input::InputKind::Read | super::input::InputKind::Request => Ok(Self::Reply),
            super::input::InputKind::Commands => Ok(Self::Request),
            super::input::InputKind::Operation => Ok(Self::Completion),
        }
    }

    fn key_direction(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Publication => "publish",
            Self::Reply => "reply",
            Self::Completion => "completion",
        }
    }

    fn expected_kind(self, input_kind: super::input::InputKind) -> crate::port::PortKind {
        match self {
            Self::Request => crate::port::PortKind::Commands,
            Self::Publication => match input_kind {
                super::input::InputKind::Latest => crate::port::PortKind::State,
                super::input::InputKind::Samples => crate::port::PortKind::Sample,
                super::input::InputKind::Events => crate::port::PortKind::Event,
                super::input::InputKind::Setpoint => crate::port::PortKind::Setpoint,
                super::input::InputKind::Stream => crate::port::PortKind::Stream,
                _ => crate::port::PortKind::State,
            },
            Self::Reply => match input_kind {
                super::input::InputKind::Read => crate::port::PortKind::Read,
                super::input::InputKind::Request => crate::port::PortKind::Commands,
                _ => crate::port::PortKind::Read,
            },
            Self::Completion => crate::port::PortKind::Commands,
        }
    }
}

fn validate_read_request_metadata(
    binding: &super::transport::PortBinding,
    allowed_callers: &BTreeMap<String, u64>,
    sample: &WireSample,
) -> crate::Result<transport::CommandIngress> {
    let metadata = sample.metadata();
    if metadata.wire_control()? != transport::WireControl::Data {
        return Err(anyhow::anyhow!(TransportError::InvalidMetadata {
            detail: format!(
                "Read request on `{}` used a stream control record",
                binding.name
            ),
        }));
    }
    let command_id = metadata.command_id.ok_or_else(|| {
        anyhow::anyhow!(TransportError::CommandCorrelation(
            "read request is missing command_id".to_owned(),
        ))
    })?;
    if metadata.eligible_boundary.is_none() {
        return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
            format!("read request {command_id} is missing eligible_boundary"),
        )));
    }
    let caller = metadata
        .caller
        .as_deref()
        .filter(|caller| !caller.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(TransportError::CommandCorrelation(format!(
                "read request {command_id} is missing caller identity"
            )))
        })?;
    let source = metadata
        .source
        .as_deref()
        .filter(|source| !source.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(TransportError::CommandCorrelation(format!(
                "read request {command_id} is missing source"
            )))
        })?;
    let ingress = transport::command_ingress(metadata)?;
    if let transport::CommandIngress::External { .. } = ingress {
        if source != "supervisor" || caller != "supervisor.public" {
            return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                format!("external Read request {command_id} is not supervisor-owned"),
            )));
        }
        return Ok(ingress);
    }
    let transport::CommandIngress::Controlled { caller_rank } = ingress else {
        unreachable!("external Read requests return above");
    };
    let (caller_instance, _) = parse_graph_endpoint(caller).map_err(|error| {
        anyhow::anyhow!(TransportError::CommandCorrelation(format!(
            "read request {command_id} has invalid caller `{caller}`: {error}"
        )))
    })?;
    if caller_instance != source {
        return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
            format!("read request {command_id} caller `{caller}` does not match source `{source}`"),
        )));
    }
    let expected_rank = allowed_callers.get(caller).ok_or_else(|| {
        anyhow::anyhow!(TransportError::CommandCorrelation(format!(
            "caller `{caller}` is not connected to Read port `{}`",
            binding.name
        )))
    })?;
    if *expected_rank != caller_rank {
        return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
            format!(
                "read request {command_id} used caller rank {caller_rank}, expected {expected_rank}"
            )
        )));
    }
    Ok(ingress)
}

#[derive(Clone, Debug)]
struct ResolvedInputRoute {
    field: &'static str,
    binding: super::transport::PortBinding,
    source_instance: String,
    source_port: String,
    direction: InputDirection,
    max_items: u64,
    max_bytes: u64,
    request_max_bytes: Option<u64>,
    caller_identity: Option<String>,
    caller_rank: Option<u64>,
}

fn parse_graph_endpoint(value: &str) -> Result<(String, String), String> {
    let (instance, port) = value
        .split_once('.')
        .ok_or_else(|| "missing instance separator".to_owned())?;
    if instance.is_empty() || port.is_empty() || port.contains('.') {
        return Err("instance and port must contain exactly one non-empty separator".to_owned());
    }
    parse_identifier(instance).map_err(|error| error.to_owned())?;
    parse_identifier(port).map_err(|error| error.to_owned())?;
    Ok((instance.to_owned(), port.to_owned()))
}

fn decode_config<C: Config>(value: Value) -> crate::Result<C> {
    match serde_json::from_value::<C>(value.clone()) {
        Ok(config) => Ok(config),
        Err(first) if value.as_object().is_some_and(serde_json::Map::is_empty) => {
            serde_json::from_value(Value::Null).map_err(|second| {
                anyhow::anyhow!(
                    "runtime configuration is not valid for the selected implementation: {first}; empty-object/unit fallback also failed: {second}"
                )
            })
        }
        Err(error) => Err(anyhow::anyhow!(
            "runtime configuration is not valid for the selected implementation: {error}"
        )),
    }
}

fn verify_executable(path: &Path, expected: &SourceExecutable) -> crate::Result<()> {
    let mut file = fs::File::open(path).map_err(|source| {
        anyhow::anyhow!(RunnerError::BundleIo {
            path: path.to_owned(),
            source,
        })
    })?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: path.to_owned(),
                source,
            })
        })?;
        if read == 0 {
            break;
        }
        bytes = bytes.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    let digest = format!("{:x}", hasher.finalize());
    if bytes != expected.bytes || digest != expected.sha256 {
        return Err(anyhow::anyhow!(RunnerError::ExecutableMismatch {
            instance: expected.instance.clone(),
        }));
    }
    Ok(())
}

fn read_bounded(path: &Path, maximum: usize) -> crate::Result<Vec<u8>> {
    let file = fs::File::open(path).map_err(|source| {
        anyhow::anyhow!(RunnerError::BundleIo {
            path: path.to_owned(),
            source,
        })
    })?;
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: path.to_owned(),
                source,
            })
        })?;
    if bytes.len() > maximum {
        return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
            message: format!("manifest.json exceeds the {maximum} byte startup bound"),
        }));
    }
    Ok(bytes)
}

fn safe_relative_path(path: &str) -> crate::Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
            message: format!("executable path `{path:?}` is not bundle-relative"),
        }));
    }
    Ok(path.to_owned())
}

fn parse_identifier(value: &str) -> Result<String, String> {
    if (1..=64).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        Ok(value.to_owned())
    } else {
        Err("instance id must be 1-64 lowercase ASCII letters, digits, '-' or '_'".to_owned())
    }
}

fn parse_execution_id(value: &str) -> Result<ExecutionId, String> {
    ExecutionId::parse(value).map_err(|error| error.to_string())
}

fn parse_endpoint(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.trim() != value
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        Err("connect endpoint must be non-empty and contain no surrounding whitespace or control characters".to_owned())
    } else {
        Ok(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    use prost::Message;

    use super::*;
    use crate::runtime::transport::RuntimeWireMetadata;
    use crate::runtime::{
        ExecutionDuration, InitContext, ObservationStamp, ReadError, RequestError, Runtime,
        RuntimeSpec, StepContext,
    };

    #[derive(Debug, Deserialize)]
    struct TestConfig {
        value: u64,
    }

    impl Config for TestConfig {
        const SCHEMA_JSON: &'static str = "{}";
    }

    fn controlled_sample(
        source: &str,
        execution_id: &str,
        timeline_id: &str,
        boundary: u64,
        item: u32,
        bytes: usize,
    ) -> WireSample {
        let metadata = RuntimeWireMetadata::data(source, ExecutionTime::default(), boundary)
            .with_delivery_identity(execution_id, timeline_id, boundary, item);
        WireSample::from_parts(vec![0; bytes], metadata, "runtime/test")
    }

    #[test]
    fn receiver_queue_keeps_unstamped_hardware_and_supervisor_ingress() {
        let mut queue = DeliveryQueue::new(4, 64, super::super::input::InputKind::Samples);
        let hardware = WireSample::from_parts(
            vec![1, 2],
            RuntimeWireMetadata::data("sensor", ExecutionTime::default(), 1),
            "runtime/sensor/ports/value/publish",
        );
        let external = WireSample::from_parts(
            vec![3],
            RuntimeWireMetadata::external_request(ExecutionTime::default(), 7, 0, 1),
            "runtime/target/ports/commands/request",
        );
        queue.admit_untracked(hardware).expect("hardware admission");
        queue.admit_untracked(external).expect("external admission");
        assert_eq!(queue.drain().len(), 2);
    }

    #[test]
    fn receiver_queue_dedupe_is_a_bounded_high_watermark() {
        let mut queue = DeliveryQueue::new(20_001, 20_001, super::super::input::InputKind::Samples);
        queue.set_timeline("timeline");
        for boundary in 0..20_000 {
            let (_, inserted) = queue
                .admit(
                    controlled_sample("producer", "execution", "timeline", boundary, 0, 1),
                    "consumer",
                    "value",
                    "publish",
                )
                .expect("controlled delivery admission");
            assert!(inserted);
        }
        assert_eq!(queue.high_watermarks.len(), 1);
        let (_, inserted) = queue
            .admit(
                controlled_sample("producer", "execution", "timeline", 19_999, 0, 1),
                "consumer",
                "value",
                "publish",
            )
            .expect("duplicate admission is idempotent");
        assert!(!inserted);
        assert_eq!(queue.items.len(), 20_000);
    }

    #[test]
    fn receiver_queue_fan_in_uses_one_aggregate_bound() {
        let mut queue = DeliveryQueue::new(2, 2, super::super::input::InputKind::Samples);
        for (source, boundary) in [("left", 0), ("right", 0)] {
            queue
                .admit(
                    controlled_sample(source, "execution", "timeline", boundary, 0, 1),
                    "consumer",
                    "value",
                    "publish",
                )
                .expect("fan-in item fits aggregate queue");
        }
        let saturated = queue.admit(
            controlled_sample("left", "execution", "timeline", 1, 0, 1),
            "consumer",
            "value",
            "publish",
        );
        assert!(matches!(
            saturated,
            Err(DeliveryAdmissionError::Saturated(_))
        ));
    }

    #[test]
    fn receiver_queue_rejects_stale_timeline_after_reset_fence() {
        let mut queue = DeliveryQueue::new(4, 4, super::super::input::InputKind::Samples);
        queue.set_timeline("timeline-1");
        let current = controlled_sample("producer", "execution", "timeline-1", 0, 0, 1);
        let identity = queue
            .identity(&current, "consumer", "value", "publish")
            .expect("identity decodes");
        assert!(queue.accepts_timeline(&identity.timeline_id));
        queue.set_timeline("timeline-2");
        assert!(!queue.accepts_timeline(&identity.timeline_id));
        assert!(queue.items.is_empty());
        assert!(queue.high_watermarks.is_empty());
    }

    #[derive(Default)]
    struct TestInputs;

    impl super::super::input::InputSet for TestInputs {
        const FIELDS: &'static [super::super::input::InputField] = &[];
    }

    impl InputSnapshot for TestInputs {
        type Transport = ();

        fn empty() -> Self {
            Self
        }
    }

    struct TestOutputs {
        value: u64,
    }

    impl super::super::outputs::OutputSet for TestOutputs {
        const FIELDS: &'static [super::super::outputs::OutputField] = &[];
    }

    struct TestRuntime {
        validate: Arc<Mutex<Vec<&'static str>>>,
        fail_step: bool,
        step_delay: Duration,
    }

    impl Runtime for TestRuntime {
        type Config = TestConfig;
        type State = u64;
        type Inputs = TestInputs;
        type Outputs = TestOutputs;

        fn validate_config(config: &Self::Config) -> crate::Result<()> {
            (config.value > 0)
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("value must be positive"))
        }

        fn init(&self, _ctx: &InitContext, config: Self::Config) -> crate::Result<Self::State> {
            self.validate.lock().expect("lock").push("init");
            Ok(config.value)
        }

        fn step(
            &self,
            _ctx: &StepContext,
            state: Self::State,
            _inputs: &Self::Inputs,
        ) -> crate::Result<(Self::State, Self::Outputs)> {
            self.validate.lock().expect("lock").push("step");
            std::thread::sleep(self.step_delay);
            if self.fail_step {
                Err(anyhow::anyhow!("step failed"))
            } else {
                Ok((state + 1, TestOutputs { value: state + 1 }))
            }
        }
    }

    impl RegisteredRuntime for TestRuntime {
        const SPEC: RuntimeSpec = RuntimeSpec::from_millis(10, 10, 100);

        fn __retain_artifact_metadata() {}
    }

    impl super::super::outputs::OutputBindings for TestRuntime {
        const FIELDS: &'static [super::super::outputs::OutputField] = &[];
    }

    struct TestInputsSource;

    impl InputSource<TestRuntime> for TestInputsSource {
        fn freeze(&mut self, _candidate: &HardwareInvocation) -> crate::Result<TestInputs> {
            Ok(TestInputs)
        }
    }

    struct TestSink {
        reserve: bool,
        published: Vec<u64>,
        stopped: bool,
    }

    impl OutputAdmission<TestOutputs> for TestSink {
        type Reservation = u64;

        fn reserve(&mut self, outputs: &TestOutputs) -> crate::Result<Self::Reservation> {
            if self.reserve {
                Ok(outputs.value)
            } else {
                Err(anyhow::anyhow!("output capacity refused"))
            }
        }
    }

    impl OutputSink<TestRuntime> for TestSink {
        fn publish(
            &mut self,
            accepted: AcceptedInvocation<TestOutputs, Self::Reservation>,
        ) -> crate::Result<()> {
            self.published.push(accepted.outputs().value);
            Ok(())
        }

        fn stop(&mut self) -> crate::Result<()> {
            self.stopped = true;
            Ok(())
        }
    }

    #[test]
    fn process_termination_boundary_cannot_return_a_terminal_operation_error() {
        let terminated = Arc::new(AtomicBool::new(false));
        let termination_flag = Arc::clone(&terminated);
        let error = anyhow::anyhow!(RunnerError::ProcessTerminationRequired {
            field: "operation",
            detail: "worker remained live".to_owned(),
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = enforce_process_boundary_with(Err(error), || {
                termination_flag.store(true, Ordering::SeqCst);
                panic!("test process terminator")
            });
        }));
        assert!(result.is_err());
        assert!(terminated.load(Ordering::SeqCst));

        let ordinary = anyhow::anyhow!("ordinary runtime failure");
        assert!(
            enforce_process_boundary_with(Err(ordinary), || {
                panic!("ordinary failures must remain returnable")
            })
            .is_err()
        );
    }

    #[test]
    fn launch_parser_requires_explicit_bundle_instance_and_endpoint() {
        let parsed = RuntimeLaunch::try_parse_from([
            "runtime",
            "--bundle-root",
            "/tmp/bundle",
            "--instance-id",
            "motion",
            "--execution-id",
            "10000000000000000000000000000001",
            "--connect",
            "tcp/127.0.0.1:7447",
        ])
        .expect("launch parses");
        assert_eq!(parsed.instance_id, "motion");
        assert_eq!(
            parsed.execution_id.to_string(),
            "10000000000000000000000000000001"
        );
        assert!(RuntimeLaunch::try_parse_from(["runtime", "--bundle-root", "/tmp"]).is_err());
    }

    #[test]
    fn runner_reserves_outputs_before_publishing_and_advancing() {
        let runtime = TestRuntime {
            validate: Arc::new(Mutex::new(Vec::new())),
            fail_step: false,
            step_delay: Duration::ZERO,
        };
        let mut runner = RuntimeRunner::new(
            runtime,
            ExecutionTime::default(),
            TestConfig { value: 1 },
            TestInputsSource,
            TestSink {
                reserve: true,
                published: Vec::new(),
                stopped: false,
            },
        )
        .expect("runner initializes");
        assert!(matches!(
            runner.poll(ExecutionTime::default()),
            Ok(PollOutcome::Accepted {
                invocation_index: 0
            })
        ));
        assert_eq!(runner.status(), RuntimeStatus::Ready);
    }

    #[test]
    fn validation_runs_before_init_for_a_non_clone_configuration() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let runtime = TestRuntime {
            validate: Arc::clone(&order),
            fail_step: false,
            step_delay: Duration::ZERO,
        };
        RuntimeRunner::new(
            runtime,
            ExecutionTime::default(),
            TestConfig { value: 1 },
            TestInputsSource,
            TestSink {
                reserve: true,
                published: Vec::new(),
                stopped: false,
            },
        )
        .expect("valid non-clone config starts");
        assert_eq!(&*order.lock().expect("lock"), &["init"]);
    }

    #[test]
    fn validation_failure_never_calls_init_for_an_owned_non_clone_config() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let runtime = TestRuntime {
            validate: Arc::clone(&order),
            fail_step: false,
            step_delay: Duration::ZERO,
        };
        assert!(
            RuntimeRunner::new(
                runtime,
                ExecutionTime::default(),
                TestConfig { value: 0 },
                TestInputsSource,
                TestSink {
                    reserve: true,
                    published: Vec::new(),
                    stopped: false,
                },
            )
            .is_err()
        );
        assert!(order.lock().expect("lock").is_empty());
    }

    #[test]
    fn output_refusal_faults_and_stops_the_runner_before_schedule_commit() {
        let runtime = TestRuntime {
            validate: Arc::new(Mutex::new(Vec::new())),
            fail_step: false,
            step_delay: Duration::ZERO,
        };
        let mut runner = RuntimeRunner::new(
            runtime,
            ExecutionTime::default(),
            TestConfig { value: 1 },
            TestInputsSource,
            TestSink {
                reserve: false,
                published: Vec::new(),
                stopped: false,
            },
        )
        .expect("runner initializes");
        assert!(runner.poll(ExecutionTime::default()).is_err());
        assert_eq!(runner.status(), RuntimeStatus::Failed);
        assert_eq!(runner.next_release(), ExecutionTime::default());
    }

    #[test]
    fn process_step_failure_is_terminal_and_does_not_publish() {
        let runtime = TestRuntime {
            validate: Arc::new(Mutex::new(Vec::new())),
            fail_step: true,
            step_delay: Duration::ZERO,
        };
        let mut runner = RuntimeRunner::new(
            runtime,
            ExecutionTime::default(),
            TestConfig { value: 1 },
            TestInputsSource,
            TestSink {
                reserve: true,
                published: Vec::new(),
                stopped: false,
            },
        )
        .expect("runner initializes");
        assert!(runner.poll(ExecutionTime::default()).is_err());
        assert_eq!(runner.status(), RuntimeStatus::Failed);
    }

    #[test]
    fn invocation_deadline_faults_the_owner_without_publishing() {
        let runtime = TestRuntime {
            validate: Arc::new(Mutex::new(Vec::new())),
            fail_step: false,
            step_delay: Duration::from_millis(20),
        };
        let mut runner = RuntimeRunner::new(
            runtime,
            ExecutionTime::default(),
            TestConfig { value: 1 },
            TestInputsSource,
            TestSink {
                reserve: true,
                published: Vec::new(),
                stopped: false,
            },
        )
        .expect("runner initializes");
        assert!(runner.poll(ExecutionTime::default()).is_err());
        assert_eq!(runner.status(), RuntimeStatus::Failed);
    }

    #[derive(Clone, PartialEq, Message)]
    struct TransportRequest {
        #[prost(uint32, tag = "1")]
        value: u32,
    }

    impl prost::Name for TransportRequest {
        const NAME: &'static str = "TransportRequest";
        const PACKAGE: &'static str = "phoxal.runtime.test";

        fn full_name() -> String {
            "phoxal.runtime.test.TransportRequest".to_owned()
        }

        fn type_url() -> String {
            "/phoxal.runtime.test.TransportRequest".to_owned()
        }
    }

    #[derive(Clone, PartialEq, Message)]
    struct TransportResponse {
        #[prost(uint32, tag = "1")]
        value: u32,
    }

    impl prost::Name for TransportResponse {
        const NAME: &'static str = "TransportResponse";
        const PACKAGE: &'static str = "phoxal.runtime.test";

        fn full_name() -> String {
            "phoxal.runtime.test.TransportResponse".to_owned()
        }

        fn type_url() -> String {
            "/phoxal.runtime.test.TransportResponse".to_owned()
        }
    }

    const TRANSPORT_PORT: crate::port::PortSignature = crate::port::PortSignature::with_descriptor(
        "transport-commands",
        "phoxal.runtime.test",
        "Transport",
        crate::port::PortKind::Commands,
        "phoxal.runtime.test.TransportRequest",
        "phoxal.runtime.test.TransportResponse",
        &[],
    );

    struct TransportInputs {
        commands: crate::runtime::Commands<TransportRequest, TransportResponse>,
    }

    impl crate::runtime::input::InputSet for TransportInputs {
        const FIELDS: &'static [crate::runtime::input::InputField] = &[];
        const TRANSPORT_FIELDS: &'static [crate::runtime::transport::InputTransportField] =
            &[crate::runtime::transport::InputTransportField {
                name: "commands",
                kind: crate::runtime::input::InputKind::Commands,
                signature: Some(TRANSPORT_PORT),
                max_age_ms: None,
                max_items: Some(4),
                max_bytes: Some(1024),
            }];

        fn decode_transport_field(
            &mut self,
            field: &str,
            mut samples: Vec<crate::runtime::transport::WireSample>,
        ) -> crate::Result<()> {
            if field != "commands" {
                return Err(anyhow::anyhow!("unexpected transport input field {field}"));
            }
            crate::runtime::transport::sort_command_samples(&mut samples)?;
            let mut items = Vec::with_capacity(samples.len());
            let mut bytes = 0_u64;
            for sample in samples {
                bytes = bytes
                    .checked_add(sample.payload().len() as u64)
                    .ok_or_else(|| {
                        anyhow::anyhow!(crate::runtime::transport::TransportError::BatchTooLarge {
                            port: TRANSPORT_PORT.name.to_owned(),
                            what: "encoded bytes",
                            actual: u64::MAX,
                            maximum: 1024,
                        })
                    })?;
                if bytes > 1024 {
                    return Err(anyhow::anyhow!(
                        crate::runtime::transport::TransportError::BatchTooLarge {
                            port: TRANSPORT_PORT.name.to_owned(),
                            what: "encoded bytes",
                            actual: bytes,
                            maximum: 1024,
                        }
                    ));
                }
                let request: TransportRequest =
                    crate::runtime::transport::decode_request(TRANSPORT_PORT, &sample, 1024)?;
                let order = crate::runtime::transport::command_order(sample.metadata())?;
                items.push(crate::runtime::Command::with_order(order, request));
            }
            self.commands = crate::runtime::Commands::bounded(
                items,
                bytes,
                crate::runtime::Capacity::new(4, 1024)?,
            )?;
            Ok(())
        }
    }

    impl InputSnapshot for TransportInputs {
        type Transport = ();

        fn empty() -> Self {
            Self {
                commands: crate::runtime::Commands::default(),
            }
        }
    }

    impl crate::runtime::input::TransportInputSet for TransportInputs {
        const TRANSPORT_FIELDS: &'static [crate::runtime::transport::InputTransportField] =
            <Self as crate::runtime::input::InputSet>::TRANSPORT_FIELDS;

        fn decode_transport_field(
            &mut self,
            field: &str,
            _binding: Option<&crate::runtime::transport::PortBinding>,
            samples: Vec<crate::runtime::transport::WireSample>,
        ) -> crate::Result<()> {
            <Self as crate::runtime::input::InputSet>::decode_transport_field(self, field, samples)
        }
    }

    impl crate::runtime::input::TransportInputSink for TransportInputs {
        fn set_latest(
            &mut self,
            field: &str,
            _value: crate::runtime::input::TransportValue,
            _stamp: ObservationStamp,
        ) -> crate::Result<()> {
            Err(anyhow::anyhow!(format!("unexpected latest field {field}")))
        }

        fn set_samples(
            &mut self,
            field: &str,
            _values: Vec<crate::runtime::input::TransportSample>,
            _gap: bool,
        ) -> crate::Result<()> {
            Err(anyhow::anyhow!(format!("unexpected samples field {field}")))
        }

        fn set_events(
            &mut self,
            field: &str,
            _values: Vec<crate::runtime::input::TransportValue>,
            _gap: bool,
        ) -> crate::Result<()> {
            Err(anyhow::anyhow!(format!("unexpected events field {field}")))
        }

        fn set_setpoint(
            &mut self,
            field: &str,
            _value: Option<(
                crate::runtime::input::TransportValue,
                ExecutionTime,
                ExecutionTime,
            )>,
        ) -> crate::Result<()> {
            Err(anyhow::anyhow!(format!(
                "unexpected setpoint field {field}"
            )))
        }

        fn set_stream(
            &mut self,
            field: &str,
            _values: Vec<crate::runtime::input::TransportStreamItem>,
        ) -> crate::Result<()> {
            Err(anyhow::anyhow!(format!("unexpected stream field {field}")))
        }

        fn set_commands(
            &mut self,
            field: &str,
            _values: Vec<crate::runtime::input::TransportCommand>,
            _encoded_bytes: u64,
        ) -> crate::Result<()> {
            Err(anyhow::anyhow!(format!(
                "unexpected commands sink field {field}"
            )))
        }

        fn set_read(
            &mut self,
            field: &str,
            _key: crate::runtime::input::TransportValue,
            _result: Result<crate::runtime::input::TransportValue, ReadError>,
        ) -> crate::Result<()> {
            Err(anyhow::anyhow!(format!("unexpected read field {field}")))
        }

        fn set_request(
            &mut self,
            field: &str,
            _key: crate::runtime::input::TransportValue,
            _result: Result<crate::runtime::input::TransportValue, RequestError>,
        ) -> crate::Result<()> {
            Err(anyhow::anyhow!(format!("unexpected request field {field}")))
        }

        fn set_operation(
            &mut self,
            field: &str,
            _key: crate::runtime::input::TransportValue,
            _result: Result<crate::runtime::input::TransportValue, OperationInputError>,
        ) -> crate::Result<()> {
            Err(anyhow::anyhow!(format!(
                "unexpected operation field {field}"
            )))
        }
    }

    #[derive(Default)]
    struct TransportOutputs {
        replies: Vec<crate::runtime::Reply<TransportResponse>>,
    }

    impl crate::runtime::outputs::OutputSet for TransportOutputs {
        const FIELDS: &'static [crate::runtime::outputs::OutputField] = &[];

        fn encode_transport(
            &self,
            context: StepContext,
            _resolve_input_port: &dyn Fn(&str) -> Option<crate::port::PortSignature>,
            source: &str,
        ) -> crate::Result<Vec<crate::runtime::transport::PreparedOutput>> {
            let mut records = Vec::with_capacity(self.replies.len());
            let mut bytes = 0_usize;
            for reply in &self.replies {
                let record = crate::runtime::transport::PreparedOutput::reply(
                    TRANSPORT_PORT,
                    reply.response(),
                    1024,
                    crate::runtime::transport::reply_metadata_for_order(
                        source,
                        context,
                        reply.order(),
                    ),
                )?;
                bytes = crate::runtime::transport::checked_add_batch_bytes(
                    TRANSPORT_PORT,
                    bytes,
                    record.payload_len(),
                    4096,
                )?;
                records.push(record);
            }
            crate::runtime::transport::check_batch(TRANSPORT_PORT, records.len(), bytes, 4, 4096)?;
            Ok(records)
        }
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct TransportRuntime;

    impl Runtime for TransportRuntime {
        type Config = ();
        type State = ();
        type Inputs = TransportInputs;
        type Outputs = TransportOutputs;

        fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
            Ok(())
        }

        fn step(
            &self,
            _ctx: &StepContext,
            state: Self::State,
            inputs: &Self::Inputs,
        ) -> crate::Result<(Self::State, Self::Outputs)> {
            let mut outputs = TransportOutputs::default();
            for command in inputs.commands.items() {
                outputs.replies.push(command.reply(TransportResponse {
                    value: command.request().value.saturating_add(1),
                }));
            }
            Ok((state, outputs))
        }
    }

    impl RegisteredRuntime for TransportRuntime {
        const SPEC: RuntimeSpec = RuntimeSpec::from_millis(10, 100, 100);

        fn __retain_artifact_metadata() {}
    }

    impl crate::runtime::outputs::OutputBindings for TransportRuntime {
        const FIELDS: &'static [crate::runtime::outputs::OutputField] = &[];
    }

    static CADENCE_FIELDS: &[crate::runtime::outputs::OutputField] =
        &[crate::runtime::outputs::OutputField {
            name: "status",
            kind: crate::runtime::outputs::OutputKind::State,
            port: Some(TRANSPORT_PORT.name),
            port_signature: Some(TRANSPORT_PORT),
            input: None,
            project: None,
            max_items: None,
            max_bytes: Some(1024),
            max_request_bytes: None,
            every_steps: Some(5),
            on_change: false,
            bootstrap: true,
            valid_for_ms: None,
            timeout_ms: None,
            cancel_grace_ms: None,
        }];

    struct CadenceRuntime;

    impl Runtime for CadenceRuntime {
        type Config = TestConfig;
        type State = u64;
        type Inputs = TestInputs;
        type Outputs = TestOutputs;

        fn init(&self, _ctx: &InitContext, config: Self::Config) -> crate::Result<Self::State> {
            Ok(config.value)
        }

        fn step(
            &self,
            _ctx: &StepContext,
            state: Self::State,
            _inputs: &Self::Inputs,
        ) -> crate::Result<(Self::State, Self::Outputs)> {
            Ok((state + 1, TestOutputs { value: state + 1 }))
        }
    }

    impl crate::runtime::outputs::OutputBindings for CadenceRuntime {
        const FIELDS: &'static [crate::runtime::outputs::OutputField] = CADENCE_FIELDS;
    }

    #[test]
    fn bootstrap_and_every_steps_use_one_based_acceptance_cadence() {
        let mut adapter = ExecutionOutputAdapter::<CadenceRuntime>::unbound();
        let output = || {
            crate::runtime::transport::PreparedOutput::response(
                TRANSPORT_PORT,
                &TransportResponse { value: 1 },
                1024,
                crate::runtime::transport::RuntimeWireMetadata::data(
                    "cadence",
                    ExecutionTime::default(),
                    1,
                ),
            )
            .expect("cadence output encodes")
            .for_field("status")
        };

        adapter.projections.push(output());
        adapter
            .filter_state_projections(
                StepContext::first(ExecutionTime::default(), ExecutionDuration::from_millis(10)),
                true,
            )
            .expect("bootstrap filters");
        assert_eq!(adapter.projections.len(), 1, "bootstrap is published first");
        adapter.projections.clear();

        for index in 0..10 {
            adapter.projections.push(output());
            adapter
                .filter_state_projections(
                    StepContext::new(
                        ExecutionTime::from_nanos((index + 1) * 10_000_000),
                        ExecutionDuration::from_millis(10),
                        ExecutionDuration::from_millis(10),
                        0,
                        index,
                    ),
                    false,
                )
                .expect("cadence filters");
            let due = matches!(index, 4 | 9);
            assert_eq!(
                adapter.projections.len(),
                usize::from(due),
                "accepted invocation {index} cadence"
            );
            adapter.projections.clear();
        }
    }

    #[test]
    fn future_commands_validate_before_retention_and_reject_replays() {
        fn sample(metadata: crate::runtime::transport::RuntimeWireMetadata) -> WireSample {
            let mut payload = Vec::new();
            TransportRequest { value: 7 }
                .encode(&mut payload)
                .expect("request encodes");
            WireSample::from_parts(
                payload,
                metadata,
                "runtime/target/ports/transport-commands/request",
            )
        }

        fn batch(sample: WireSample) -> CollectedInput {
            CollectedInput {
                field: "commands",
                binding: crate::runtime::transport::PortBinding::from_signature(TRANSPORT_PORT),
                direction: InputDirection::Request,
                max_items: 4,
                max_bytes: 1024,
                samples: vec![sample],
            }
        }

        let mut adapter = ExecutionInputAdapter::<TransportRuntime>::unbound();
        let mut malformed = crate::runtime::transport::RuntimeWireMetadata::external_command(
            ExecutionTime::default(),
            1,
            100,
            1,
        );
        malformed.eligible_boundary = None;
        let error = adapter
            .validate_future_command_admission(&batch(sample(malformed)))
            .expect_err("future records validate before retention");
        assert!(error.to_string().contains("eligible_boundary"));
        assert!(adapter.future_commands.is_empty());

        let valid = sample(
            crate::runtime::transport::RuntimeWireMetadata::external_command(
                ExecutionTime::default(),
                2,
                100,
                2,
            ),
        );
        adapter
            .future_commands
            .insert("commands", vec![valid.clone()]);
        let error = adapter
            .validate_future_command_admission(&batch(valid))
            .expect_err("replayed retained future records are rejected");
        assert!(error.to_string().contains("duplicate command id"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn generated_prost_runtime_transport_round_trip_preserves_command_order() {
        use zenoh::Wait;
        use zenoh::bytes::Encoding;
        use zenoh::key_expr::OwnedKeyExpr;

        let (owner, bus) =
            crate::bus::session::BusOwner::open(crate::bus::BusConfig::for_participant(
                crate::identity::ExecutionId::mint(),
                crate::identity::ParticipantId::new("typed-runtime").expect("participant id"),
                Vec::new(),
            ))
            .await
            .expect("test bus opens");
        let mut input = ExecutionInputAdapter::<TransportRuntime>::unbound();
        input
            .bind_direct(bus.clone(), "transport")
            .await
            .expect("input binds");
        let mut output = ExecutionOutputAdapter::<TransportRuntime>::unbound();
        output.bind_direct(bus.clone(), "transport");

        let session = bus.session().expect("session is open");
        let reply_key = bus.full_key(&crate::runtime::transport::port_key(
            "transport",
            TRANSPORT_PORT.name,
            "reply",
        ));
        let replies = session
            .declare_subscriber(OwnedKeyExpr::new(reply_key).expect("reply key"))
            .with(zenoh::handlers::FifoChannel::new(4))
            .await
            .expect("reply subscriber");

        let input_key = bus.full_key(&crate::runtime::transport::port_key(
            "transport",
            TRANSPORT_PORT.name,
            "request",
        ));
        let publish_request = |request: TransportRequest,
                               source: &str,
                               command_id: u64,
                               eligible_boundary: u64,
                               caller_rank: u64| {
            let mut payload = Vec::new();
            request.encode(&mut payload).expect("request encodes");
            let metadata = crate::runtime::transport::RuntimeWireMetadata::command(
                source,
                ExecutionTime::from_nanos(10),
                command_id,
                eligible_boundary,
                caller_rank,
            )
            .with_caller(format!("{source}.commands"))
            .encode_bounded()
            .expect("metadata encodes");
            session
                .put(input_key.clone(), payload)
                .encoding(Encoding::from(
                    crate::runtime::transport::PROTOBUF_ENCODING.to_owned(),
                ))
                .attachment(metadata)
                .wait()
                .expect("request publishes");
        };
        let publish_external_request =
            |request: TransportRequest,
             command_id: u64,
             eligible_boundary: u64,
             ingress_sequence: u64| {
                let mut payload = Vec::new();
                request.encode(&mut payload).expect("request encodes");
                let metadata = crate::runtime::transport::RuntimeWireMetadata::external_command(
                    ExecutionTime::from_nanos(10),
                    command_id,
                    eligible_boundary,
                    ingress_sequence,
                )
                .encode_bounded()
                .expect("metadata encodes");
                session
                    .put(input_key.clone(), payload)
                    .encoding(Encoding::from(
                        crate::runtime::transport::PROTOBUF_ENCODING.to_owned(),
                    ))
                    .attachment(metadata)
                    .wait()
                    .expect("external request publishes");
            };
        // Publish the higher-ranked caller first.  The frozen input cut must
        // still use the authoritative boundary/rank merge key, not Zenoh
        // arrival order.
        publish_request(TransportRequest { value: 50 }, "caller-b", 100, 0, 2);
        publish_request(TransportRequest { value: 41 }, "caller-a", 99, 0, 3);
        publish_external_request(TransportRequest { value: 60 }, 101, 0, 3);

        let mut runner = RuntimeRunner::new(
            TransportRuntime,
            ExecutionTime::default(),
            (),
            input,
            output,
        )
        .expect("runtime initializes");
        let first_poll = runner.poll(ExecutionTime::default());
        assert!(matches!(
            first_poll,
            Ok(PollOutcome::Accepted {
                invocation_index: 0
            })
        ));

        let mut received = Vec::new();
        for expected in [(100, 2, 51), (99, 3, 42), (101, 0, 61)] {
            let sample = tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
                .await
                .expect("reply arrives")
                .expect("reply receive succeeds");
            let wire = crate::runtime::transport::WireSample::from_zenoh(sample)
                .expect("reply has typed transport metadata");
            let response = TransportResponse::decode(wire.payload()).expect("response decodes");
            assert_eq!(response.value, expected.2);
            assert_eq!(wire.metadata().command_id, Some(expected.0));
            assert_eq!(wire.metadata().eligible_boundary, Some(0));
            if expected.0 == 101 {
                assert_eq!(wire.metadata().caller, Some("supervisor.public".to_owned()));
                assert_eq!(wire.metadata().ingress_sequence, Some(3));
                assert_eq!(wire.metadata().caller_rank, None);
            } else {
                assert_eq!(wire.metadata().caller_rank, Some(expected.1));
            }
            received.push(wire);
        }
        assert_eq!(received.len(), 3);

        // A request for a later eligible boundary remains retained without
        // entering the earlier frozen cuts, then becomes visible exactly at
        // its boundary.
        publish_external_request(TransportRequest { value: 70 }, 102, 17, 4);
        for index in 1..=17 {
            let now = ExecutionTime::from_nanos(index * 10_000_000);
            assert!(matches!(
                runner.poll(now),
                Ok(PollOutcome::Accepted { invocation_index }) if invocation_index == index
            ));
            if index < 17 {
                assert!(
                    replies
                        .try_recv()
                        .expect("reply receive succeeds")
                        .is_none()
                );
            }
        }
        let future_reply = tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
            .await
            .expect("future reply arrives")
            .expect("future reply receive succeeds");
        let future_wire = crate::runtime::transport::WireSample::from_zenoh(future_reply)
            .expect("future reply has typed metadata");
        assert_eq!(future_wire.metadata().command_id, Some(102));
        assert_eq!(future_wire.metadata().eligible_boundary, Some(17));
        assert_eq!(future_wire.metadata().ingress_sequence, Some(4));

        // A late replay from the already admitted command must fail before a
        // second service step and must not produce a duplicate reply.
        publish_request(TransportRequest { value: 999 }, "caller-a", 99, 0, 3);
        assert!(runner.poll(ExecutionTime::from_nanos(10_000_000)).is_err());
        assert_eq!(runner.status(), RuntimeStatus::Failed);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), replies.recv_async())
                .await
                .is_err()
        );

        owner.close().await;
    }

    #[crate::runtime::inputs]
    struct OperationInputs {
        operation: crate::runtime::Operation<u64, u32>,
    }

    struct OperationRuntime {
        activate_once: Arc<AtomicBool>,
        completion: Arc<Mutex<Option<u32>>>,
    }

    impl Runtime for OperationRuntime {
        type Config = ();
        type State = ();
        type Inputs = OperationInputs;
        type Outputs = ();

        fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
            Ok(())
        }

        fn step(
            &self,
            _ctx: &StepContext,
            state: Self::State,
            inputs: &Self::Inputs,
        ) -> crate::Result<(Self::State, Self::Outputs)> {
            if let Some(completion) = inputs.operation.new_completion() {
                let value = completion
                    .result()
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                *self.completion.lock().expect("operation completion lock") = Some(*value);
            }
            Ok((state, ()))
        }
    }

    impl RegisteredRuntime for OperationRuntime {
        const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

        fn __retain_artifact_metadata() {}
    }

    #[crate::runtime::outputs]
    impl OperationRuntime {
        #[crate::runtime::outputs::activate(operation)]
        fn activate(&self, _state: &()) -> Option<crate::runtime::Activation<u64, u32>> {
            (!self.activate_once.swap(true, Ordering::AcqRel))
                .then(|| crate::runtime::Activation::new(7, 41))
        }

        #[crate::runtime::outputs::operation(operation, timeout_ms = 100, cancel_grace_ms = 20)]
        fn run(input: u32) -> crate::Result<u32> {
            Ok(input + 1)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn generated_operation_activation_dispatches_and_returns_typed_completion()
    -> crate::Result<()> {
        let (owner, bus) =
            crate::bus::session::BusOwner::open(crate::bus::BusConfig::for_participant(
                crate::identity::ExecutionId::mint(),
                crate::identity::ParticipantId::new("typed-operation").expect("participant id"),
                Vec::new(),
            ))
            .await
            .expect("test bus opens");
        let correlations = Arc::new(Mutex::new(BTreeMap::new()));
        let expired_correlations = Arc::new(Mutex::new(BTreeSet::new()));
        let operation_completions = Arc::new(Mutex::new(Vec::new()));
        let exchange_completions = Arc::new(Mutex::new(Vec::new()));
        let mut input = ExecutionInputAdapter::<OperationRuntime>::unbound().with_shared_state(
            Arc::clone(&correlations),
            Arc::clone(&expired_correlations),
            Arc::clone(&operation_completions),
            Arc::clone(&exchange_completions),
        );
        input
            .bind_direct(bus.clone(), "operation")
            .await
            .expect("input binds");
        let mut output = ExecutionOutputAdapter::<OperationRuntime>::unbound().with_shared_state(
            correlations,
            expired_correlations,
            operation_completions,
            exchange_completions,
        );
        output.bind_direct(bus.clone(), "operation");
        let completion = Arc::new(Mutex::new(None));
        let mut runner = RuntimeRunner::new(
            OperationRuntime {
                activate_once: Arc::new(AtomicBool::new(false)),
                completion: Arc::clone(&completion),
            },
            ExecutionTime::default(),
            (),
            input,
            output,
        )
        .expect("runtime initializes");

        assert!(matches!(
            runner.poll(ExecutionTime::default()),
            Ok(PollOutcome::Accepted {
                invocation_index: 0
            })
        ));
        for index in 1..=20 {
            tokio::time::sleep(Duration::from_millis(2)).await;
            let now = ExecutionTime::from_nanos(index * 2_000_000);
            let _ = runner.poll(now)?;
            if completion
                .lock()
                .expect("operation completion lock")
                .is_some()
            {
                break;
            }
        }
        assert_eq!(
            *completion.lock().expect("operation completion lock"),
            Some(42)
        );
        runner.stop().expect("runner stops");
        owner.close().await;
        Ok(())
    }

    #[derive(Clone, PartialEq, Message)]
    struct ReadRequest {
        #[prost(uint32, tag = "1")]
        value: u32,
    }

    impl prost::Name for ReadRequest {
        const NAME: &'static str = "ReadRequest";
        const PACKAGE: &'static str = "phoxal.runtime.test";

        fn full_name() -> String {
            "phoxal.runtime.test.ReadRequest".to_owned()
        }

        fn type_url() -> String {
            "/phoxal.runtime.test.ReadRequest".to_owned()
        }
    }

    #[derive(Clone, PartialEq, Message)]
    struct ReadResponse {
        #[prost(uint32, tag = "1")]
        value: u32,
    }

    impl prost::Name for ReadResponse {
        const NAME: &'static str = "ReadResponse";
        const PACKAGE: &'static str = "phoxal.runtime.test";

        fn full_name() -> String {
            "phoxal.runtime.test.ReadResponse".to_owned()
        }

        fn type_url() -> String {
            "/phoxal.runtime.test.ReadResponse".to_owned()
        }
    }

    const READ_PORT: crate::port::Read<ReadRequest, ReadResponse> =
        crate::port::Read::with_signature(
            "read",
            "phoxal.runtime.test.Reader",
            "Current",
            "phoxal.runtime.test.ReadRequest",
            "phoxal.runtime.test.ReadResponse",
            &[],
        );

    struct PublicReadRuntime;

    impl Runtime for PublicReadRuntime {
        type Config = ();
        type State = u32;
        type Inputs = ReadClientInputs;
        type Outputs = ();

        fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
            Ok(41)
        }

        fn step(
            &self,
            _ctx: &StepContext,
            state: Self::State,
            _inputs: &Self::Inputs,
        ) -> crate::Result<(Self::State, Self::Outputs)> {
            Ok((state, ()))
        }
    }

    impl RegisteredRuntime for PublicReadRuntime {
        const SPEC: RuntimeSpec = RuntimeSpec::from_millis(10, 100, 100);

        fn __retain_artifact_metadata() {}
    }

    #[crate::runtime::outputs]
    impl PublicReadRuntime {
        fn view(&self, state: &u32) -> u32 {
            *state
        }

        #[crate::runtime::outputs::read(
            port = READ_PORT,
            project = Self::view,
            max_request_bytes = 64,
            max_response_bytes = 64,
        )]
        fn inspect(&self, view: &u32, request: &ReadRequest) -> ReadResponse {
            ReadResponse {
                value: view.saturating_add(request.value),
            }
        }
    }

    #[crate::runtime::inputs]
    struct ReadClientInputs {
        #[crate::runtime::input(max_response_bytes = 64)]
        read: crate::runtime::Read<u64, ReadRequest, ReadResponse>,
    }

    struct ReadClientRuntime {
        activate_once: Arc<AtomicBool>,
        response: Arc<Mutex<Option<u32>>>,
    }

    impl Runtime for ReadClientRuntime {
        type Config = ();
        type State = ();
        type Inputs = ReadClientInputs;
        type Outputs = ();

        fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
            Ok(())
        }

        fn step(
            &self,
            _ctx: &StepContext,
            state: Self::State,
            inputs: &Self::Inputs,
        ) -> crate::Result<(Self::State, Self::Outputs)> {
            if let Some(completion) = inputs.read.new_completion() {
                let response = completion
                    .result()
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                *self.response.lock().expect("read response lock") = Some(response.value);
            }
            Ok((state, ()))
        }
    }

    impl RegisteredRuntime for ReadClientRuntime {
        const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

        fn __retain_artifact_metadata() {}
    }

    #[crate::runtime::outputs]
    impl ReadClientRuntime {
        #[crate::runtime::outputs::activate(read, timeout_ms = 100)]
        fn request(&self, _state: &()) -> Option<crate::runtime::Activation<u64, ReadRequest>> {
            (!self.activate_once.swap(true, Ordering::AcqRel))
                .then(|| crate::runtime::Activation::new(9, ReadRequest { value: 41 }))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn generated_read_activation_uses_graph_target_and_correlated_reply() -> crate::Result<()>
    {
        use zenoh::Wait;
        use zenoh::bytes::Encoding;
        use zenoh::key_expr::OwnedKeyExpr;

        let (owner, bus) =
            crate::bus::session::BusOwner::open(crate::bus::BusConfig::for_participant(
                crate::identity::ExecutionId::mint(),
                crate::identity::ParticipantId::new("read-client").expect("participant id"),
                Vec::new(),
            ))
            .await
            .expect("test bus opens");
        let signature = SourcePortSignature {
            name: READ_PORT.signature().name.to_owned(),
            service: READ_PORT.signature().service.to_owned(),
            method: READ_PORT.signature().method.to_owned(),
            kind: READ_PORT.signature().kind.as_str().to_owned(),
            request: READ_PORT.signature().request.to_owned(),
            response: READ_PORT.signature().response.to_owned(),
        };
        let mut connections = BTreeMap::new();
        connections.insert(
            "read-client.read".to_owned(),
            vec!["reader.read".to_owned()],
        );
        let mut artifacts = BTreeMap::new();
        artifacts.insert(
            "reader".to_owned(),
            SourceRuntimeRecord {
                period_ms: Some(1),
                timeout_ms: Some(100),
                init_timeout_ms: Some(100),
                inputs: Vec::new(),
                transient_outputs: Vec::new(),
                service_outputs: vec![SourceOutputRecord {
                    name: "current".to_owned(),
                    kind: "read".to_owned(),
                    port: Some("read".to_owned()),
                    signature: Some(signature),
                    input: None,
                    max_items: Some(1),
                    max_bytes: Some(64),
                    max_request_bytes: Some(64),
                }],
            },
        );
        let manifest = RuntimeLaunchManifest {
            root: PathBuf::from("."),
            robot_id: "typed-read-test".to_owned(),
            instance_id: "read-client".to_owned(),
            executable: PathBuf::from("typed-read-test"),
            executable_sha256: "00".repeat(32),
            config: Value::Object(serde_json::Map::new()),
            connections,
            artifacts,
        };
        // The input and output adapters must share one correlation state.
        let correlations = Arc::new(Mutex::new(BTreeMap::new()));
        let expired = Arc::new(Mutex::new(BTreeSet::new()));
        let operations = Arc::new(Mutex::new(Vec::new()));
        let exchanges = Arc::new(Mutex::new(Vec::new()));
        let mut input = ExecutionInputAdapter::<ReadClientRuntime>::unbound().with_shared_state(
            Arc::clone(&correlations),
            Arc::clone(&expired),
            Arc::clone(&operations),
            Arc::clone(&exchanges),
        );
        input.bind(bus.clone(), &manifest).await?;
        let mut output = ExecutionOutputAdapter::<ReadClientRuntime>::unbound().with_shared_state(
            correlations,
            expired,
            operations,
            exchanges,
        );
        output.bind(bus.clone(), "read-client", &manifest).await?;

        let session = bus.session().expect("session is open");
        let request_key = bus.full_key(&crate::runtime::transport::port_key(
            "reader",
            READ_PORT.name(),
            "request",
        ));
        let requests = session
            .declare_subscriber(OwnedKeyExpr::new(request_key).expect("request key"))
            .with(zenoh::handlers::FifoChannel::new(2))
            .await
            .expect("request subscriber");
        let response = Arc::new(Mutex::new(None));
        let mut runner = RuntimeRunner::new(
            ReadClientRuntime {
                activate_once: Arc::new(AtomicBool::new(false)),
                response: Arc::clone(&response),
            },
            ExecutionTime::default(),
            (),
            input,
            output,
        )?;
        assert!(matches!(
            runner.poll(ExecutionTime::default()),
            Ok(PollOutcome::Accepted {
                invocation_index: 0
            })
        ));
        let request = tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
            .await
            .expect("read request arrives")
            .expect("read request receive succeeds");
        let wire = crate::runtime::transport::WireSample::from_zenoh(request)?;
        let request = ReadRequest::decode(wire.payload()).expect("request decodes");
        assert_eq!(request.value, 41);
        let metadata = wire.metadata();
        let payload = crate::runtime::transport::encode_prost(&ReadResponse { value: 42 })?;
        let reply_metadata = crate::runtime::transport::reply_metadata(
            "reader",
            StepContext::first(
                ExecutionTime::from_nanos(2_000_000),
                ExecutionDuration::from_millis(1),
            ),
            metadata.command_id.expect("command id"),
            metadata.eligible_boundary.expect("boundary"),
            metadata.caller_rank.expect("caller rank"),
        )
        .encode_bounded()
        .expect("reply metadata");
        let response_key = bus.full_key(&crate::runtime::transport::port_key(
            "reader",
            READ_PORT.name(),
            "reply",
        ));
        session
            .put(response_key, payload)
            .encoding(Encoding::from(
                crate::runtime::transport::PROTOBUF_ENCODING.to_owned(),
            ))
            .attachment(reply_metadata)
            .wait()
            .expect("reply publishes");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = runner.poll(ExecutionTime::from_nanos(2_000_000))?;
        assert_eq!(*response.lock().expect("read response lock"), Some(42));
        runner.stop()?;
        owner.close().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn public_read_uses_authenticated_external_ingress() -> crate::Result<()> {
        use zenoh::Wait;
        use zenoh::bytes::Encoding;
        use zenoh::key_expr::OwnedKeyExpr;

        let (owner, bus) =
            crate::bus::session::BusOwner::open(crate::bus::BusConfig::for_participant(
                crate::identity::ExecutionId::mint(),
                crate::identity::ParticipantId::new("public-reader").expect("participant id"),
                Vec::new(),
            ))
            .await
            .expect("test bus opens");
        let manifest = RuntimeLaunchManifest {
            root: PathBuf::from("."),
            robot_id: "public-read-test".to_owned(),
            instance_id: "public-reader".to_owned(),
            executable: PathBuf::from("public-read-test"),
            executable_sha256: "00".repeat(32),
            config: Value::Object(serde_json::Map::new()),
            connections: BTreeMap::new(),
            artifacts: BTreeMap::new(),
        };
        let mut input = ExecutionInputAdapter::<PublicReadRuntime>::unbound();
        input.bind_direct(bus.clone(), "public-reader").await?;
        let mut output = ExecutionOutputAdapter::<PublicReadRuntime>::unbound();
        output.bind(bus.clone(), "public-reader", &manifest).await?;

        let session = bus.session().expect("session is open");
        let reply_key = bus.full_key(&crate::runtime::transport::port_key(
            "public-reader",
            READ_PORT.name(),
            "reply",
        ));
        let replies = session
            .declare_subscriber(OwnedKeyExpr::new(reply_key).expect("reply key"))
            .with(zenoh::handlers::FifoChannel::new(2))
            .await
            .expect("reply subscriber");
        let request_key = bus.full_key(&crate::runtime::transport::port_key(
            "public-reader",
            READ_PORT.name(),
            "request",
        ));
        let mut request_payload = Vec::new();
        ReadRequest { value: 1 }.encode(&mut request_payload)?;
        let request_metadata = crate::runtime::transport::RuntimeWireMetadata::external_request(
            ExecutionTime::default(),
            7,
            0,
            12,
        )
        .encode_bounded()
        .expect("external read metadata");
        session
            .put(request_key, request_payload)
            .encoding(Encoding::from(
                crate::runtime::transport::PROTOBUF_ENCODING.to_owned(),
            ))
            .attachment(request_metadata)
            .wait()
            .expect("external read publishes");

        let mut runner = RuntimeRunner::new(
            PublicReadRuntime,
            ExecutionTime::default(),
            (),
            input,
            output,
        )?;
        assert!(matches!(
            runner.poll(ExecutionTime::default()),
            Ok(PollOutcome::Accepted {
                invocation_index: 0
            })
        ));
        let reply = tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
            .await
            .expect("public read reply arrives")
            .expect("public read reply receive succeeds");
        let wire = crate::runtime::transport::WireSample::from_zenoh(reply)?;
        let response = ReadResponse::decode(wire.payload()).expect("response decodes");
        assert_eq!(response.value, 42);
        assert_eq!(wire.metadata().source.as_deref(), Some("public-reader"));
        assert_eq!(wire.metadata().caller.as_deref(), Some("supervisor.public"));
        assert_eq!(wire.metadata().caller_rank, None);
        assert_eq!(wire.metadata().ingress_sequence, Some(12));
        runner.stop()?;
        owner.close().await;
        Ok(())
    }

    #[derive(Clone, PartialEq, Message)]
    struct TypedState {
        #[prost(int32, tag = "1")]
        value: i32,
    }

    impl prost::Name for TypedState {
        const NAME: &'static str = "TypedState";
        const PACKAGE: &'static str = "phoxal.runtime.test";

        fn full_name() -> String {
            "phoxal.runtime.test.TypedState".to_owned()
        }

        fn type_url() -> String {
            "/phoxal.runtime.test.TypedState".to_owned()
        }
    }

    const SOURCE_STATE: crate::port::PortSignature = crate::port::PortSignature::with_descriptor(
        "state",
        "phoxal.runtime.test",
        "State",
        crate::port::PortKind::State,
        "google.protobuf.Empty",
        "phoxal.runtime.test.TypedState",
        &[],
    );

    #[crate::runtime::inputs]
    struct TypedStateInputs {
        state: crate::runtime::Latest<TypedState>,
    }

    struct TypedStateRuntime {
        seen: Arc<Mutex<Option<i32>>>,
    }

    impl Runtime for TypedStateRuntime {
        type Config = ();
        type State = ();
        type Inputs = TypedStateInputs;
        type Outputs = ();

        fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
            Ok(())
        }

        fn step(
            &self,
            _ctx: &StepContext,
            state: Self::State,
            inputs: &Self::Inputs,
        ) -> crate::Result<(Self::State, Self::Outputs)> {
            let value = inputs
                .state
                .value()
                .ok_or_else(|| anyhow::anyhow!("typed state was not admitted"))?;
            *self.seen.lock().expect("typed state observation lock") = Some(value.value);
            Ok((state, ()))
        }
    }

    impl RegisteredRuntime for TypedStateRuntime {
        const SPEC: RuntimeSpec = RuntimeSpec::from_millis(10, 100, 100);

        fn __retain_artifact_metadata() {}
    }

    impl crate::runtime::outputs::OutputBindings for TypedStateRuntime {
        const FIELDS: &'static [crate::runtime::outputs::OutputField] = &[];
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn generated_nonempty_state_transport_uses_manifest_connection() -> crate::Result<()> {
        let (owner, bus) =
            crate::bus::session::BusOwner::open(crate::bus::BusConfig::for_participant(
                crate::identity::ExecutionId::mint(),
                crate::identity::ParticipantId::new("typed-state").expect("participant id"),
                Vec::new(),
            ))
            .await
            .expect("test bus opens");
        let signature = SourcePortSignature {
            name: SOURCE_STATE.name.to_owned(),
            service: SOURCE_STATE.service.to_owned(),
            method: SOURCE_STATE.method.to_owned(),
            kind: SOURCE_STATE.kind.as_str().to_owned(),
            request: SOURCE_STATE.request.to_owned(),
            response: SOURCE_STATE.response.to_owned(),
        };
        let mut connections = BTreeMap::new();
        connections.insert(
            "consumer.state".to_owned(),
            vec!["producer.state".to_owned()],
        );
        let mut artifacts = BTreeMap::new();
        artifacts.insert(
            "producer".to_owned(),
            SourceRuntimeRecord {
                period_ms: Some(10),
                timeout_ms: Some(100),
                init_timeout_ms: Some(100),
                inputs: Vec::new(),
                transient_outputs: Vec::new(),
                service_outputs: vec![SourceOutputRecord {
                    name: "state".to_owned(),
                    kind: "state".to_owned(),
                    port: Some("state".to_owned()),
                    signature: Some(signature),
                    input: None,
                    max_items: Some(1),
                    max_bytes: Some(64),
                    max_request_bytes: None,
                }],
            },
        );
        let manifest = RuntimeLaunchManifest {
            root: PathBuf::from("."),
            robot_id: "typed-test".to_owned(),
            instance_id: "consumer".to_owned(),
            executable: PathBuf::from("typed-test"),
            executable_sha256: "00".repeat(32),
            config: Value::Object(serde_json::Map::new()),
            connections,
            artifacts,
        };
        let mut input = ExecutionInputAdapter::<TypedStateRuntime>::unbound();
        input
            .bind(bus.clone(), &manifest)
            .await
            .expect("input binds");
        let mut output = ExecutionOutputAdapter::<TypedStateRuntime>::unbound();
        output.bind_direct(bus.clone(), "consumer");

        let prepared = PreparedOutput::response(
            SOURCE_STATE,
            &TypedState { value: 42 },
            64,
            crate::runtime::transport::publication_metadata(
                "producer",
                StepContext::first(ExecutionTime::default(), ExecutionDuration::from_millis(10)),
                0,
            ),
        )?;
        crate::runtime::transport::publish_batch(&bus, "producer", &[prepared])?;
        let seen = Arc::new(Mutex::new(None));
        let mut runner = RuntimeRunner::new(
            TypedStateRuntime { seen: seen.clone() },
            ExecutionTime::default(),
            (),
            input,
            output,
        )
        .expect("runtime initializes");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(matches!(
            runner.poll(ExecutionTime::default()),
            Ok(PollOutcome::Accepted {
                invocation_index: 0
            })
        ));
        assert_eq!(
            *seen.lock().expect("typed state observation lock"),
            Some(42)
        );
        runner.stop().expect("runner stops");
        owner.close().await;
        Ok(())
    }
}
