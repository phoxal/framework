//! The bounded process runner for the synchronous [`super::Runtime`] contract.
//!
//! The runner owns the process lifecycle around a runtime owner.  It selects
//! one newest-due hardware release, freezes one input cut, executes the step
//! under its host deadline, reserves all output capacity, and only then
//! advances the schedule.  Input and output transport remain explicit host
//! implementations because only a contract owner knows how to encode its
//! generated payloads.

mod exchange;
mod input;
mod output;
mod read;
use input::ExecutionInputAdapter;
use output::ExecutionOutputAdapter;
#[cfg(test)]
mod tests;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use zenoh::bytes::Encoding;
use zenoh::key_expr::OwnedKeyExpr;

use super::core::{AcceptedInvocation, Config, OutputAdmission, RegisteredRuntime, RuntimeOwner};
use super::execution_protocol::{self, wire as execution_wire};
use super::input::{InputSnapshot, TransportInputSet};
use super::schedule::{HardwareInvocation, HardwareSchedule, ScheduleError};
use super::transport::{self, TransportError, WireSample};
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
    /// Consumer input field in the compiled runtime contract.
    pub input: String,
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
    observation_providers: BTreeMap<(String, String), SourceObservationProvider>,
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
        } else if let Some(service) = manifest.document.services.get(instance_id) {
            service
                .config
                .clone()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
        } else if let Some(component) = manifest.document.robot.components.get(instance_id) {
            let driver = component
                .driver
                .as_ref()
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    anyhow::anyhow!(RunnerError::BundleInvalid {
                        message: format!(
                            "executable `{instance_id}` has no configured component driver binding"
                        ),
                    })
                })?;
            driver
                .get("config")
                .cloned()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
        } else {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!(
                    "executable `{instance_id}` has no matching service or component driver entry"
                ),
            }));
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
            observation_providers: manifest
                .simulation
                .into_iter()
                .flat_map(|simulation| simulation.providers)
                .map(|provider| {
                    (
                        (provider.service_instance.clone(), provider.port.clone()),
                        provider,
                    )
                })
                .collect(),
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
                let virtual_provider = self
                    .observation_providers
                    .get(&(source_instance.clone(), source_port.clone()));
                let (binding, source_max_bytes, source_max_items, source_request_max_bytes) =
                    if let Some(provider) = virtual_provider {
                        if direction != InputDirection::Publication {
                            anyhow::bail!("simulation provider `{source}` cannot serve requests");
                        }
                        (
                            provider.binding()?,
                            Some(u64::from(provider.max_message_bytes)),
                            Some(u64::from(provider.max_buffered_items)),
                            None,
                        )
                    } else {
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
                        .transient_outputs
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
                        (
                            binding,
                            source_max_bytes,
                            source_max_items,
                            source_request_max_bytes,
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

    /// Fence Read state before controlled traffic can enter a new timeline.
    fn set_read_timeline(&mut self, _timeline: &str) -> crate::Result<()> {
        Ok(())
    }

    /// Pin accepted immutable projections at controlled boundary entry.
    fn pin_read_views(&mut self, _timeline: &str, _boundary: u64) -> crate::Result<()> {
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

    /// Prepare owned Read views without publishing them before acceptance.
    fn prepare_read_views(
        &mut self,
        _service: std::sync::Arc<R>,
        _state: &R::State,
        _context: StepContext,
    ) -> crate::Result<()> {
        Ok(())
    }

    /// Expose the accepted immutable Read views independently of telemetry.
    fn commit_read_views(&mut self) -> crate::Result<()> {
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
        result = crate::runtime::connection::ConnectionOwner::open(crate::runtime::connection::ConnectionConfig::for_participant(
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
        match RuntimeRunner::new_deferred(service, ExecutionTime::default(), config, input, output)
        {
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
    let initialize_subscriber =
        declare_execution_subscriber(&bus, &launch.instance_id, "initialize-state").await?;
    let pin_subscriber =
        declare_execution_subscriber(&bus, &launch.instance_id, "pin-read-views").await?;
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
    if execution_mode == execution_wire::ExecutionMode::Hardware {
        runner.bootstrap(ExecutionTime::default())?;
    }
    if execution_mode == execution_wire::ExecutionMode::Controlled {
        // Install the input identity before acknowledging readiness: initial
        // observations may arrive immediately after the host sees Ready.
        runner.set_controlled_timeline(&admission.timeline_id)?;
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
                    initialize_subscriber,
                    pin_subscriber,
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
    bus: &crate::runtime::connection::Connection,
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
    bus: &crate::runtime::connection::Connection,
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
    let exact_capabilities = execution_protocol::REQUIRED_CAPABILITIES
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
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
    if requirement.protocol != "phoxal.execution.v1"
        || capabilities != exact_capabilities
        || requirement.capabilities.len() != exact_capabilities.len()
    {
        unsupported_contracts.push(requirement.protocol.clone());
        unsupported_contracts.extend(
            capabilities
                .difference(&exact_capabilities)
                .map(|capability| format!("phoxal.execution.v1/{capability}")),
        );
        return reject(
            "execution admission requires the exact current execution capabilities".to_owned(),
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
    bus: &'a crate::runtime::connection::Connection,
    invoke_subscriber: ExecutionSubscriber,
    reset_subscriber: ExecutionSubscriber,
    initialize_subscriber: ExecutionSubscriber,
    pin_subscriber: ExecutionSubscriber,
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
        initialize_subscriber,
        pin_subscriber,
        mut timeline_id,
        quantum_ns,
    } = transport;
    let mut last_boundary = None;
    let mut initialized_state: Option<execution_wire::InitializeStateResponse> = None;
    let mut accepted = BTreeMap::<u64, execution_wire::InvocationAccepted>::new();
    loop {
        tokio::select! {
            biased;
            _ = shutdown.as_mut() => break Ok(()),
            pin = pin_subscriber.recv_async() => {
                let sample = pin.map_err(|error| anyhow::anyhow!(error.to_string()))?;
                let request = decode_execution_sample::<execution_wire::PinReadViewsRequest>(&sample)?;
                if request.execution_id != bus.execution().to_string() || request.timeline_id != timeline_id { continue; }
                let result = runner.outputs.pin_read_views(&timeline_id, request.boundary);
                let response = execution_wire::PinReadViewsResponse {
                    execution_id: request.execution_id, timeline_id: request.timeline_id,
                    boundary: request.boundary, runtime_instance: launch.instance_id.clone(),
                    admitted: result.is_ok(), detail: result.err().map(|error| error.to_string()),
                };
                publish_execution(bus, &launch.instance_id, "pin-read-views-response", &response).await?;
            }
            initialize = initialize_subscriber.recv_async() => {
                let sample = initialize.map_err(|error| anyhow::anyhow!(error.to_string()))?;
                let request = decode_execution_sample::<execution_wire::InitializeStateRequest>(&sample)?;
                if request.execution_id != bus.execution().to_string() || request.timeline_id != timeline_id {
                    continue;
                }
                if initialized_state.is_none() {
                    if last_boundary.is_some() {
                        break Err(anyhow::anyhow!("initialized State must precede the first invocation"));
                    }
                    runner.initialize_controlled_state(&timeline_id)?;
                    initialized_state = Some(execution_wire::InitializeStateResponse {
                        execution_id: request.execution_id,
                        timeline_id: request.timeline_id,
                        runtime_instance: launch.instance_id.clone(),
                        required_products: runner.outputs.take_product_receipts().into_iter().map(|receipt| execution_wire::ProductReceipt {
                            port: receipt.port, sequence: receipt.sequence, items: receipt.items, bytes: receipt.bytes,
                        }).collect(),
                        required_deliveries: runner.outputs.take_delivery_receipts().into_iter().map(|receipt| execution_wire::DeliveryReceipt {
                            port: receipt.port, direction: receipt.direction, target: receipt.target.unwrap_or_default(), sequence: receipt.sequence, item: receipt.item, bytes: receipt.bytes,
                        }).collect(),
                    });
                }
                if let Some(response) = &initialized_state {
                    publish_execution(bus, &launch.instance_id, "initialize-state-response", response).await?;
                }
            }
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
                        initialized_state = None;
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
                if initialized_state.is_none() {
                    break Err(anyhow::anyhow!("controlled invocation preceded initialized State admission"));
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
                            input: receipt.input,
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
    bus: &crate::runtime::connection::Connection,
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
    controlled: bool,
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
        let mut runner = Self::new_deferred(service, now, config, inputs, outputs)?;
        runner.bootstrap(now)?;
        Ok(runner)
    }

    fn new_deferred(
        service: R,
        now: ExecutionTime,
        config: R::Config,
        inputs: Inputs,
        mut outputs: Outputs,
    ) -> crate::Result<Self> {
        let initialization_started = Instant::now();
        let owner = RuntimeOwner::new(service, now, config)?;
        if initialization_started.elapsed() > R::SPEC.init_timeout.as_duration() {
            return Err(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        if let Some(state) = owner.state_ref() {
            outputs.prepare_read_views(
                owner.shared_service(),
                state,
                StepContext::first(now, R::SPEC.period),
            )?;
            outputs.commit_read_views()?;
        }
        let schedule = HardwareSchedule::new(now, R::SPEC.period)
            .map_err(|error| anyhow::anyhow!(RunnerError::Schedule(error)))?;
        Ok(Self {
            owner,
            schedule,
            inputs,
            outputs,
            stopped: false,
            controlled_previous: None,
            controlled: false,
        })
    }

    fn bootstrap(&mut self, now: ExecutionTime) -> crate::Result<()> {
        if let Some(state) = self.owner.state_ref() {
            self.outputs.bootstrap(self.owner.service(), state, now)?;
        }
        Ok(())
    }

    fn initialize_controlled_state(&mut self, timeline_id: &str) -> crate::Result<()> {
        self.outputs.prepare_delivery(0, timeline_id)?;
        self.bootstrap(ExecutionTime::default())
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
            let service = self.owner.shared_service();
            let owner = &mut self.owner;
            let outputs = &mut self.outputs;
            match owner.accept_with_hook(
                &candidate.context(),
                &inputs,
                outputs,
                |_, state, context, outputs| {
                    outputs.prepare_read_views(service.clone(), state, *context)?;
                    outputs.prepare_state(&service, state, context)
                },
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
            return Err(anyhow::anyhow!(
                crate::runtime::connection::ConnectionError::Closed
            ));
        }
        let context = StepContext::from_previous(
            now,
            R::SPEC.period,
            self.controlled_previous,
            0,
            self.owner.next_invocation().index(),
        );
        let candidate = HardwareInvocation::controlled(context, boundary);
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
            let service = self.owner.shared_service();
            let owner = &mut self.owner;
            let outputs = &mut self.outputs;
            match owner.accept_with_hook(
                &candidate.context(),
                &inputs,
                outputs,
                |_, state, context, outputs| {
                    outputs.prepare_read_views(service.clone(), state, *context)?;
                    outputs.prepare_state(&service, state, context)
                },
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
        self.inputs.set_timeline(timeline_id)?;
        self.outputs.set_read_timeline(timeline_id)?;
        self.controlled = true;
        Ok(())
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
        if let Some(state) = self.owner.state_ref() {
            self.outputs.prepare_read_views(
                self.owner.shared_service(),
                state,
                StepContext::first(now, R::SPEC.period),
            )?;
            self.outputs.commit_read_views()?;
        }
        if !self.controlled
            && let Err(error) = self.bootstrap(now)
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
    #[serde(default)]
    simulation: Option<SourceSimulation>,
}

#[derive(Debug, Deserialize)]
struct SourceSimulation {
    providers: Vec<SourceObservationProvider>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct SourceObservationProvider {
    service_instance: String,
    port: String,
    service_fqn: String,
    method: String,
    kind: String,
    input_fqn: String,
    payload_fqn: String,
    max_message_bytes: u32,
    max_buffered_items: u32,
}

impl SourceObservationProvider {
    fn binding(&self) -> crate::Result<super::transport::PortBinding> {
        SourcePortSignature {
            name: self.port.clone(),
            service: self.service_fqn.clone(),
            method: self.method.clone(),
            kind: self.kind.clone(),
            request: self.input_fqn.clone(),
            response: self.payload_fqn.clone(),
        }
        .to_binding()
    }
}

#[derive(Debug, Deserialize, Default)]
struct SourceDocument {
    #[serde(default)]
    robot: SourceRobot,
    #[serde(default)]
    services: BTreeMap<String, SourceService>,
    #[serde(default)]
    connections: BTreeMap<String, SourceConnectionSources>,
}

#[derive(Debug, Deserialize, Default)]
struct SourceRobot {
    #[serde(default)]
    components: BTreeMap<String, SourceComponent>,
}

#[derive(Debug, Deserialize, Default)]
struct SourceComponent {
    #[serde(default)]
    driver: Option<Value>,
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
