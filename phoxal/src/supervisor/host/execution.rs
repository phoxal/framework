//! Supervisor-owned execution admission and controlled boundary protocol.
//!
//! A public simulation advance is not complete when a local counter changes.
//! This module sends one exact invocation to every due runtime, waits for the
//! corresponding acceptance and product receipts, and commits the supervisor
//! boundary only after the whole required roster has replied.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use prost::Message;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use zenoh::bytes::Encoding;
use zenoh::key_expr::OwnedKeyExpr;

use crate::bus::BusHandle;
use crate::communication::simulation::{
    AcquireAuthorityRequest, AdvanceRequest, AdvanceResponse, ProgressRequest, ProgressResponse,
    ReleaseAuthorityRequest, ResetRequest,
};
use crate::communication_transport::PublicSimulationContext;
use crate::runtime::execution_protocol::{self, wire};
use crate::supervisor::api::time_domain::TimeMode;

use super::bundle::SourceBundle;
use super::public_backend::RuntimeBoundaryHook;
use super::state::ExecutionState;

const CONTROL_CHANNEL_CAPACITY: usize = 64;
const DEFAULT_RUNTIME_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PRODUCT_RECEIPTS: usize = 4096;
const ADMISSION_RETRY: Duration = Duration::from_millis(100);

type BoundaryFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send>>;

type Subscriber =
    zenoh::pubsub::Subscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeExecutionMode {
    Hardware,
    Controlled,
}

impl RuntimeExecutionMode {
    fn wire(self) -> wire::ExecutionMode {
        match self {
            Self::Hardware => wire::ExecutionMode::Hardware,
            Self::Controlled => wire::ExecutionMode::Controlled,
        }
    }
}

struct RuntimeInstance {
    instance: String,
    digest: Vec<u8>,
    period_ns: u64,
    timeout: Duration,
    input_ports: BTreeSet<String>,
    input_sources: BTreeMap<String, BTreeSet<(String, String)>>,
    product_ports: BTreeSet<String>,
    actuation_ports: BTreeSet<String>,
    actuation_max_bytes: BTreeMap<String, u64>,
    admit_response: Subscriber,
    ready: Subscriber,
    accepted: Subscriber,
    failures: Subscriber,
    reset_response: Subscriber,
    delivery_ack: Subscriber,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ExpectedDelivery {
    source: String,
    target: String,
    port: String,
    direction: String,
    sequence: u64,
    item: u32,
    bytes: u64,
}

struct BoundaryState {
    mode: RuntimeExecutionMode,
    quantum_ns: u64,
    timeline_id: String,
    fault: Option<String>,
    committed_boundary: u64,
}

struct ProtocolInner {
    bus: BusHandle,
    execution_id: String,
    state: ExecutionState,
    instances: Vec<RuntimeInstance>,
    artifacts: BTreeMap<String, ArtifactRuntime>,
    connections: BTreeMap<String, serde_json::Value>,
    delivery_routes: BTreeMap<(String, String, String), BTreeSet<String>>,
    request_routes: BTreeSet<(String, String, String)>,
    boundary: Mutex<BoundaryState>,
    failed: CancellationToken,
}

/// Supervisor-side owner of private runtime admission and controlled progress.
#[derive(Clone)]
pub(crate) struct RuntimeExecutionProtocol {
    inner: Arc<ProtocolInner>,
}

impl std::fmt::Debug for RuntimeExecutionProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeExecutionProtocol")
            .field("execution_id", &self.inner.execution_id)
            .field("instances", &self.inner.instances.len())
            .finish_non_exhaustive()
    }
}

impl RuntimeExecutionProtocol {
    /// Declare response subscribers before any child process is launched.
    pub(crate) async fn open(
        bus: BusHandle,
        source: &SourceBundle,
        state: ExecutionState,
    ) -> Result<Self> {
        let mut instances = Vec::new();
        let mut artifacts = BTreeMap::<String, ArtifactRuntime>::new();
        for executable in source.executables() {
            let instance = executable.instance().to_owned();
            let digest = decode_digest(executable.sha256())
                .with_context(|| format!("runtime `{instance}` has an invalid executable digest"))?;
            let artifact = source
                .artifact(&instance)
                .with_context(|| format!("runtime `{instance}` has no artifact contract"))?;
            let artifact: ArtifactSummary = serde_json::from_value(artifact.clone())
                .with_context(|| format!("runtime `{instance}` artifact contract is invalid"))?;
            artifacts.insert(instance.clone(), artifact.runtime.clone());
            let period_ms = artifact
                .runtime
                .period_ms
                .with_context(|| format!("runtime `{instance}` has no authored period"))?;
            let timeout_ms = artifact.runtime.timeout_ms.unwrap_or(0);
            let period_ns = period_ms.checked_mul(1_000_000).with_context(|| {
                format!("runtime `{instance}` authored period overflows nanoseconds")
            })?;
            if period_ns == 0 {
                bail!("runtime `{instance}` authored period must be positive");
            }
            let timeout = if timeout_ms == 0 {
                DEFAULT_RUNTIME_TIMEOUT
            } else {
                Duration::from_millis(timeout_ms)
            };
            let input_ports = artifact
                .runtime
                .inputs
                .iter()
                .map(|input| input.port.clone().unwrap_or_else(|| input.name.clone()))
                .collect::<BTreeSet<_>>();
            let input_sources = input_sources(source, &instance, &input_ports)?;
            let mut product_ports = artifact
                .runtime
                .transient_outputs
                .iter()
                .chain(artifact.runtime.service_outputs.iter())
                .map(|output| output.port.clone().unwrap_or_else(|| output.name.clone()))
                .collect::<BTreeSet<_>>();
            // A Commands reply is emitted through the target command input's
            // descriptor even though the reply output record has no public
            // `port` field.  Keep that exact endpoint admissible for the
            // compact product summary when a reply is actually produced.
            for input in &artifact.runtime.inputs {
                if input.kind == "commands"
                    && artifact.runtime.service_outputs.iter().any(|output| {
                        output.kind == "reply"
                            && output.input.as_deref() == Some(input.name.as_str())
                    })
                {
                    product_ports.insert(input.port.clone().unwrap_or_else(|| input.name.clone()));
                }
            }
            let actuation_ports = artifact
                .runtime
                .transient_outputs
                .iter()
                .chain(artifact.runtime.service_outputs.iter())
                .filter(|output| output.kind == "setpoint")
                .map(|output| output.port.clone().unwrap_or_else(|| output.name.clone()))
                .collect::<BTreeSet<_>>();
            let actuation_max_bytes = artifact
                .runtime
                .transient_outputs
                .iter()
                .chain(artifact.runtime.service_outputs.iter())
                .filter(|output| output.kind == "setpoint")
                .filter_map(|output| {
                    output
                        .port
                        .as_ref()
                        .or(Some(&output.name))
                        .map(|port| (port.clone(), output.max_bytes.unwrap_or(u64::MAX)))
                })
                .collect::<BTreeMap<_, _>>();
            let admit_response = declare(&bus, &instance, "admit-response").await?;
            let ready = declare(&bus, &instance, "ready").await?;
            let accepted = declare(&bus, &instance, "accepted").await?;
            let failures = declare(&bus, &instance, "failure").await?;
            let reset_response = declare(&bus, &instance, "reset-response").await?;
            let delivery_ack = declare(&bus, &instance, "delivery-ack").await?;
            instances.push(RuntimeInstance {
                instance,
                digest,
                period_ns,
                timeout,
                input_ports,
                input_sources,
                product_ports,
                actuation_ports,
                actuation_max_bytes,
                admit_response,
                ready,
                accepted,
                failures,
                reset_response,
                delivery_ack,
            });
        }
        let (delivery_routes, request_routes) =
            graph_delivery_routes(source.connections(), &artifacts)?;
        Ok(Self {
            inner: Arc::new(ProtocolInner {
                execution_id: bus.execution().to_string(),
                bus,
                state: state.clone(),
                instances,
                artifacts,
                connections: source.connections().clone(),
                delivery_routes,
                request_routes,
                boundary: Mutex::new(BoundaryState {
                    mode: RuntimeExecutionMode::Hardware,
                    quantum_ns: 0,
                    timeline_id: state.time_domain().timeline.to_string(),
                    fault: None,
                    committed_boundary: state.runtime_boundary(),
                }),
                failed: CancellationToken::new(),
            }),
        })
    }

    /// Admit every required runtime with one selected scheduling mode.
    pub(crate) async fn admit_all(
        &self,
        mode: RuntimeExecutionMode,
        quantum_ns: u64,
        timeline_id: &str,
    ) -> Result<()> {
        if matches!(mode, RuntimeExecutionMode::Controlled) && quantum_ns == 0 {
            bail!("controlled execution requires a positive quantum");
        }
        if matches!(mode, RuntimeExecutionMode::Hardware) && quantum_ns != 0 {
            bail!("hardware execution cannot carry a simulation quantum");
        }
        if matches!(mode, RuntimeExecutionMode::Controlled) {
            validate_controlled_capacity(
                &self.inner.instances,
                &self.inner.artifacts,
                &self.inner.connections,
                quantum_ns,
            )?;
        }
        for runtime in &self.inner.instances {
            if matches!(mode, RuntimeExecutionMode::Controlled)
                && !runtime.period_ns.is_multiple_of(quantum_ns)
            {
                bail!(
                    "runtime `{}` period {} ns is not an exact multiple of quantum {} ns",
                    runtime.instance,
                    runtime.period_ns,
                    quantum_ns
                );
            }
            let request = self.admission_request(runtime, mode, quantum_ns, timeline_id);
            send(&self.inner.bus, &runtime.instance, "admit", &request).await?;
        }
        for runtime in &self.inner.instances {
            let started = tokio::time::Instant::now();
            let response = loop {
                let remaining = crate::supervisor::host::process::STARTUP_TIMEOUT
                    .checked_sub(started.elapsed())
                    .with_context(|| format!("runtime `{}` admission timed out", runtime.instance))?;
                match tokio::time::timeout(remaining.min(ADMISSION_RETRY), recv_admission(&runtime.admit_response)).await {
                    Ok(response) => break response?,
                    Err(_) => {
                        let request = self.admission_request(runtime, mode, quantum_ns, timeline_id);
                        send(&self.inner.bus, &runtime.instance, "admit", &request).await?;
                    }
                }
            };
            if !response.admitted {
                bail!(
                    "runtime `{}` refused execution admission: {}",
                    runtime.instance,
                    response.detail.unwrap_or_else(|| "unspecified refusal".to_owned())
                );
            }
            if !response.unsupported_contracts.is_empty() {
                bail!(
                    "runtime `{}` returned unsupported contracts after admission: {}",
                    runtime.instance,
                    response.unsupported_contracts.join(", ")
                );
            }
        }
        for runtime in &self.inner.instances {
            let ready = tokio::time::timeout(
                crate::supervisor::host::process::STARTUP_TIMEOUT,
                recv_ready(&runtime.ready),
            )
            .await
            .with_context(|| format!("runtime `{}` Ready timed out", runtime.instance))??;
            if ready.execution_id != self.inner.execution_id
                || ready.timeline_id != timeline_id
                || ready.runtime_instance != runtime.instance
            {
                bail!(
                    "runtime `{}` published a Ready message for another execution",
                    runtime.instance
                );
            }
        }
        let mut boundary = self.inner.boundary.lock().await;
        boundary.mode = mode;
        boundary.quantum_ns = quantum_ns;
        boundary.timeline_id = timeline_id.to_owned();
        boundary.committed_boundary = self.inner.state.runtime_boundary();
        boundary.fault = None;
        Ok(())
    }

    fn admission_request(
        &self,
        runtime: &RuntimeInstance,
        mode: RuntimeExecutionMode,
        quantum_ns: u64,
        timeline_id: &str,
    ) -> wire::AdmitExecutionRequest {
        wire::AdmitExecutionRequest {
            execution_id: self.inner.execution_id.clone(),
            timeline_id: timeline_id.to_owned(),
            artifact_digest: runtime.digest.clone(),
            required_contracts: vec![wire::ContractRequirement {
                protocol: "phoxal.execution.v1".to_owned(),
                capabilities: vec![
                    "invocation".to_owned(),
                    "reset".to_owned(),
                    "delivery-ack".to_owned(),
                ],
            }],
            mode: mode.wire() as i32,
            quantum_ns,
        }
    }

    pub(crate) async fn wait_failed(&self) {
        self.inner.failed.cancelled().await;
    }

    pub(crate) async fn failure_reason(&self) -> Option<String> {
        self.inner.boundary.lock().await.fault.clone()
    }

    async fn advance_inner(
        &self,
        context: PublicSimulationContext,
        request: AdvanceRequest,
        published_observations: Vec<crate::communication::simulation::ProductReceipt>,
    ) -> Result<AdvanceResponse, String> {
        let mut boundary = self.inner.boundary.lock().await;
        if let Some(fault) = &boundary.fault {
            return Err(format!("controlled execution has failed: {fault}"));
        }
        if boundary.mode != RuntimeExecutionMode::Controlled {
            return Err("hardware execution has no controlled simulation barrier".to_owned());
        }
        if context.execution_id != self.inner.execution_id
            || request.execution_id != self.inner.execution_id
            || context.timeline_id != boundary.timeline_id
            || request.timeline_id != boundary.timeline_id
            || request.boundary != boundary.committed_boundary
            || context.completed_boundary != request.boundary
        {
            return Err("controlled advance identity or boundary is stale".to_owned());
        }
        if let Err(error) = validate_published_observations(&request, &published_observations) {
            return self.fail_boundary(&mut boundary, request.boundary, error);
        }
        let Some(target) = request.boundary.checked_add(1) else {
            return self.fail_boundary(
                &mut boundary,
                request.boundary,
                "controlled boundary is exhausted".to_owned(),
            );
        };
        let Some(logical_time_ns) = request.boundary.checked_mul(boundary.quantum_ns) else {
            return self.fail_boundary(
                &mut boundary,
                request.boundary,
                "controlled logical time is exhausted".to_owned(),
            );
        };
        let due = self
            .inner
            .instances
            .iter()
            .filter(|runtime| {
                request.boundary == 0
                    || logical_time_ns.is_multiple_of(runtime.period_ns)
            })
            .collect::<Vec<_>>();
        for runtime in &due {
            let invocation = wire::Invocation {
                execution_id: self.inner.execution_id.clone(),
                timeline_id: boundary.timeline_id.clone(),
                runtime_instance: runtime.instance.clone(),
                boundary: request.boundary,
                logical_time_ns,
            };
            if let Err(error) = send(&self.inner.bus, &runtime.instance, "invoke", &invocation).await {
                return self.fail_boundary(&mut boundary, request.boundary, error.to_string());
            }
        }
        let mut product_receipt_count = 0_usize;
        let mut actuations = Vec::new();
        for runtime in due {
            let timeout = runtime.timeout.max(Duration::from_millis(1));
            let accepted = match tokio::time::timeout(
                timeout,
                recv_acceptance(
                    &runtime.accepted,
                    &runtime.failures,
                    &self.inner.execution_id,
                    &boundary.timeline_id,
                    &runtime.instance,
                    request.boundary,
                    &runtime.product_ports,
                ),
            )
            .await
            {
                Ok(Ok(accepted)) => accepted,
                Ok(Err(error)) => {
                    return self.fail_boundary(&mut boundary, request.boundary, error);
                }
                Err(_) => {
                    return self.fail_boundary(
                        &mut boundary,
                        request.boundary,
                        format!("runtime `{}` invocation timed out", runtime.instance),
                    );
                }
            };
            product_receipt_count = match product_receipt_count
                .checked_add(accepted.required_products.len())
            {
                Some(count) => count,
                None => {
                    return self.fail_boundary(
                        &mut boundary,
                        request.boundary,
                        "required product receipt count overflow".to_owned(),
                    );
                }
            };
            if product_receipt_count > MAX_PRODUCT_RECEIPTS {
                return self.fail_boundary(
                    &mut boundary,
                    request.boundary,
                    "required product receipt set is too large".to_owned(),
                );
            }
            if let Err(error) = validate_products(&accepted, &runtime.product_ports) {
                return self.fail_boundary(&mut boundary, request.boundary, error);
            }
            if let Err(error) = validate_input_receipts(&accepted, runtime) {
                return self.fail_boundary(&mut boundary, request.boundary, error);
            }
            let accepted_actuations = match validate_actuations(
                &accepted,
                runtime,
                logical_time_ns,
            ) {
                Ok(actuations) => actuations,
                Err(error) => {
                    return self.fail_boundary(&mut boundary, request.boundary, error);
                }
            };
            let deliveries = match expected_deliveries(
                runtime,
                &accepted,
                &self.inner.delivery_routes,
                &self.inner.request_routes,
            ) {
                Ok(deliveries) => deliveries,
                Err(error) => {
                    return self.fail_boundary(&mut boundary, request.boundary, error);
                }
            };
            if let Err(error) = wait_delivery_acknowledgements(
                &runtime.delivery_ack,
                &runtime.failures,
                &self.inner.execution_id,
                &boundary.timeline_id,
                &runtime.instance,
                request.boundary,
                &deliveries,
                timeout,
            )
            .await
            {
                return self.fail_boundary(&mut boundary, request.boundary, error);
            }
            actuations.extend(accepted_actuations.into_iter().map(|actuation| {
                crate::communication::simulation::Actuation {
                    service_instance: runtime.instance.clone(),
                    port: actuation.port,
                    payload: actuation.payload,
                    valid_until_ns: actuation.valid_until_ns,
                }
            }));
        }
        let response = AdvanceResponse {
            completed_boundary: target,
            actuation: actuations,
            observation_receipts: published_observations,
            session_id: request.session_id,
            execution_id: self.inner.execution_id.clone(),
            timeline_id: boundary.timeline_id.clone(),
            requested_boundary: request.boundary,
            correlation_id: request.correlation_id,
            authority_grant: context.authority_grant,
        };
        if let Err(error) = self.inner.state.complete_runtime_boundary(target) {
            return self.fail_boundary(&mut boundary, request.boundary, error.to_string());
        }
        boundary.committed_boundary = target;
        Ok(response)
    }

    fn fail_boundary(
        &self,
        boundary: &mut BoundaryState,
        at: u64,
        reason: String,
    ) -> Result<AdvanceResponse, String> {
        boundary.fault = Some(format!("boundary {at}: {reason}"));
        self.inner.failed.cancel();
        Err(boundary.fault.clone().unwrap_or(reason))
    }

    fn fail_reset(&self, boundary: &mut BoundaryState, reason: String) -> Result<(), String> {
        boundary.fault = Some(format!("reset failed: {reason}"));
        self.inner.failed.cancel();
        Err(boundary.fault.clone().unwrap_or(reason))
    }

    async fn reset_inner(
        &self,
        context: PublicSimulationContext,
        request: ResetRequest,
        next_timeline_id: String,
    ) -> Result<(), String> {
        let mut boundary = self.inner.boundary.lock().await;
        if boundary.mode != RuntimeExecutionMode::Controlled {
            return Err("hardware execution refuses controlled reset".to_owned());
        }
        if boundary.fault.is_some()
            || context.execution_id != self.inner.execution_id
            || request.execution_id != self.inner.execution_id
            || request.timeline_id != boundary.timeline_id
            || request.completed_boundary != boundary.committed_boundary
            || context.completed_boundary != request.completed_boundary
            || next_timeline_id.is_empty()
            || next_timeline_id == boundary.timeline_id
        {
            return Err("reset identity, timeline, or boundary is stale".to_owned());
        }
        let timeline = parse_timeline_id(&next_timeline_id)
            .map_err(|error| format!("reset timeline is invalid: {error}"))?;
        for runtime in &self.inner.instances {
            let reset = wire::ResetExecutionRequest {
                execution_id: self.inner.execution_id.clone(),
                retired_timeline_id: boundary.timeline_id.clone(),
                next_timeline_id: next_timeline_id.clone(),
                completed_boundary: boundary.committed_boundary,
            };
            if let Err(error) = send(&self.inner.bus, &runtime.instance, "reset", &reset).await {
                return self.fail_reset(&mut boundary, error.to_string());
            }
            let response = match tokio::time::timeout(
                runtime.timeout.max(Duration::from_millis(1)),
                recv_reset(&runtime.reset_response),
            )
            .await
            {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => return self.fail_reset(&mut boundary, error.to_string()),
                Err(_) => {
                    return self.fail_reset(
                        &mut boundary,
                        format!("runtime `{}` reset timed out", runtime.instance),
                    );
                }
            };
            if !response.accepted {
                return self.fail_reset(
                    &mut boundary,
                    format!(
                        "runtime `{}` refused reset: {}",
                        runtime.instance,
                        response.detail.unwrap_or_else(|| "unspecified refusal".to_owned())
                    ),
                );
            }
        }
        if let Err(error) = self
            .inner
            .state
            .replace_time_domain_with(TimeMode::Simulated, timeline)
        {
            return self.fail_reset(&mut boundary, error.to_string());
        }
        self.inner.state.reset_runtime_boundary();
        boundary.timeline_id = next_timeline_id;
        boundary.committed_boundary = 0;
        Ok(())
    }
}

fn input_sources(
    source: &SourceBundle,
    consumer_instance: &str,
    input_ports: &BTreeSet<String>,
) -> Result<BTreeMap<String, BTreeSet<(String, String)>>> {
    let mut providers = BTreeMap::<String, BTreeSet<(String, String)>>::new();
    for (consumer, sources) in source.connections() {
        let Some((instance, port)) = consumer.split_once('.') else {
            bail!("connection consumer `{consumer}` has no field separator");
        };
        if instance != consumer_instance || !input_ports.contains(port) {
            continue;
        }
        let source_values = match sources {
            serde_json::Value::String(source) => vec![source.as_str()],
            serde_json::Value::Array(sources) => sources
                .iter()
                .map(|source| {
                    source.as_str().ok_or_else(|| {
                        anyhow::anyhow!("connection `{consumer}` contains a non-string source")
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            _ => bail!("connection `{consumer}` must contain a source or source list"),
        };
        for source in source_values {
            let Some((source_instance, source_port)) = source.split_once('.') else {
                bail!("connection source `{source}` has no port separator");
            };
            providers
                    .entry(port.to_owned())
                    .or_default()
                    .insert((source_instance.to_owned(), source_port.to_owned()));
        }
    }
    Ok(providers)
}

fn connection_source_values<'a>(
    consumer: &str,
    value: &'a serde_json::Value,
) -> Result<Vec<&'a str>> {
    match value {
        serde_json::Value::String(source) => Ok(vec![source.as_str()]),
        serde_json::Value::Array(sources) => sources
            .iter()
            .map(|source| {
                source.as_str().with_context(|| {
                    format!("connection `{consumer}` contains a non-string source")
                })
            })
            .collect(),
        _ => bail!("connection `{consumer}` must contain a source or source list"),
    }
}

fn graph_delivery_routes(
    connections: &BTreeMap<String, serde_json::Value>,
    artifacts: &BTreeMap<String, ArtifactRuntime>,
) -> Result<(
    BTreeMap<(String, String, String), BTreeSet<String>>,
    BTreeSet<(String, String, String)>,
)> {
    let mut routes = BTreeMap::<(String, String, String), BTreeSet<String>>::new();
    let mut request_routes = BTreeSet::<(String, String, String)>::new();
    for (consumer, source_values) in connections {
        let (consumer_instance, consumer_port) = consumer
            .split_once('.')
            .with_context(|| format!("connection consumer `{consumer}` has no port separator"))?;
        if consumer_instance.is_empty() || consumer_port.is_empty() {
            bail!("connection consumer `{consumer}` has an empty instance or port");
        }
        let consumer_artifact = artifacts
            .get(consumer_instance)
            .with_context(|| format!("connection consumer `{consumer}` has no runtime artifact"))?;
        let input = consumer_artifact
            .inputs
            .iter()
            .find(|input| {
                input.name == consumer_port || input.port.as_deref() == Some(consumer_port)
            });
        let kind = input.map(|input| input.kind.as_str()).unwrap_or_default();
        for source in connection_source_values(consumer, source_values)? {
            let (source_instance, source_port) = source
                .split_once('.')
                .with_context(|| format!("connection source `{source}` has no port separator"))?;
            if source_instance.is_empty() || source_port.is_empty() {
                bail!("connection source `{source}` has an empty instance or port");
            }
            match kind {
                "commands" => {
                    request_routes.insert((
                        consumer_instance.to_owned(),
                        source_instance.to_owned(),
                        source_port.to_owned(),
                    ));
                }
                "read" | "request" => {
                    routes
                        .entry((
                            source_instance.to_owned(),
                            source_port.to_owned(),
                            "reply".to_owned(),
                        ))
                        .or_default()
                        .insert(consumer_instance.to_owned());
                }
                _ => {
                    routes
                        .entry((
                            source_instance.to_owned(),
                            source_port.to_owned(),
                            "publish".to_owned(),
                        ))
                        .or_default()
                        .insert(consumer_instance.to_owned());
                }
            }
        }
    }
    Ok((routes, request_routes))
}

fn validate_controlled_capacity(
    instances: &[RuntimeInstance],
    artifacts: &BTreeMap<String, ArtifactRuntime>,
    connections: &BTreeMap<String, serde_json::Value>,
    quantum_ns: u64,
) -> Result<()> {
    let periods = instances
        .iter()
        .map(|runtime| (runtime.instance.as_str(), runtime.period_ns))
        .collect::<BTreeMap<_, _>>();
    for (consumer, source_values) in connections {
        let (consumer_instance, consumer_port) = consumer
            .split_once('.')
            .with_context(|| format!("connection consumer `{consumer}` has no port separator"))?;
        let consumer_artifact = artifacts
            .get(consumer_instance)
            .with_context(|| format!("connection consumer `{consumer}` has no runtime artifact"))?;
        let Some(input) = consumer_artifact.inputs.iter().find(|input| {
            input.name == consumer_port || input.port.as_deref() == Some(consumer_port)
        }) else {
            // The source compiler may retain a connection for an operation
            // field, which has no receiver queue reservation.
            continue;
        };
        if matches!(input.kind.as_str(), "operation" | "") {
            continue;
        }
        let consumer_period = *periods
            .get(consumer_instance)
            .with_context(|| format!("consumer `{consumer_instance}` has no admitted period"))?;
        let consumer_stride = consumer_period
            .checked_div(quantum_ns)
            .filter(|stride| *stride > 0)
            .with_context(|| format!("consumer `{consumer_instance}` period is not quantum-aligned"))?;
        let replaceable = matches!(input.kind.as_str(), "latest" | "setpoint");
        let mut required_items = 0_u64;
        let mut required_bytes = 0_u64;
        for source in connection_source_values(consumer, source_values)? {
            let (source_instance, source_port) = source
                .split_once('.')
                .with_context(|| format!("connection source `{source}` has no port separator"))?;
            let source_period = *periods
                .get(source_instance)
                .with_context(|| format!("source `{source_instance}` has no admitted period"))?;
            let source_stride = source_period
                .checked_div(quantum_ns)
                .filter(|stride| *stride > 0)
                .with_context(|| format!("source `{source_instance}` period is not quantum-aligned"))?;
            let source_artifact = artifacts
                .get(source_instance)
                .with_context(|| format!("source `{source}` has no runtime artifact"))?;
            let source_input = source_artifact.inputs.iter().find(|candidate| {
                candidate.port.as_deref() == Some(source_port)
                    && candidate.kind == "commands"
            });
            let source_output = source_artifact
                .transient_outputs
                .iter()
                .chain(source_artifact.service_outputs.iter())
                .find(|output| output.port.as_deref() == Some(source_port));
            let (batch_items, batch_bytes, every_steps, bootstrap) = if let Some(input) = source_input
            {
                // A Commands endpoint is a receiver queue on the target
                // runtime.  Each connected caller can contribute at most one
                // activation per due invocation, bounded by the target input.
                (
                    input.max_items.unwrap_or(1),
                    input.max_bytes.unwrap_or(1),
                    1,
                    false,
                )
            } else if let Some(output) = source_output {
                (
                    output.max_items.unwrap_or(1),
                    output.max_bytes.or(output.max_request_bytes).unwrap_or(1),
                    output.every_steps.unwrap_or(1),
                    output.bootstrap,
                )
            } else {
                (1, 1, 1, false)
            };
            if every_steps == 0 {
                bail!("output `{source}` has a zero every_steps cadence");
            }
            let source_invocations = consumer_stride
                .checked_add(source_stride.saturating_sub(1))
                .and_then(|value| value.checked_div(source_stride))
                .unwrap_or(u64::MAX)
                .max(1);
            let emitted_batches = source_invocations
                .checked_add(every_steps.saturating_sub(1))
                .and_then(|value| value.checked_div(every_steps))
                .unwrap_or(u64::MAX)
                .saturating_add(u64::from(bootstrap));
            let route_items = batch_items.saturating_mul(emitted_batches);
            let route_bytes = batch_bytes.saturating_mul(emitted_batches);
            if replaceable {
                required_items = required_items.max(route_items.min(1));
                required_bytes = required_bytes.max(route_bytes.min(batch_bytes));
            } else {
                required_items = required_items.saturating_add(route_items);
                required_bytes = required_bytes.saturating_add(route_bytes);
            }
        }
        let available_items = input.max_items.unwrap_or(required_items.max(1));
        let available_bytes = input.max_bytes.unwrap_or(required_bytes.max(1));
        if required_items > available_items || required_bytes > available_bytes {
            bail!(
                "controlled receiver `{consumer}` capacity is insufficient: requires {required_items} items/{required_bytes} bytes but reserves {available_items} items/{available_bytes} bytes"
            );
        }
    }
    Ok(())
}

impl RuntimeBoundaryHook for RuntimeExecutionProtocol {
    fn acquire(
        &self,
        context: PublicSimulationContext,
        _request: AcquireAuthorityRequest,
    ) -> BoundaryFuture<()> {
        let protocol = self.clone();
        Box::pin(async move {
            let boundary = protocol.inner.boundary.lock().await;
            if boundary.mode != RuntimeExecutionMode::Controlled
                || context.execution_id != protocol.inner.execution_id
                || context.timeline_id != boundary.timeline_id
            {
                return Err("simulation authority requires a controlled admitted execution".to_owned());
            }
            if let Some(fault) = &boundary.fault {
                return Err(format!("controlled execution has failed: {fault}"));
            }
            Ok(())
        })
    }

    fn advance(
        &self,
        context: PublicSimulationContext,
        request: AdvanceRequest,
        published_observations: Vec<crate::communication::simulation::ProductReceipt>,
    ) -> BoundaryFuture<AdvanceResponse> {
        let protocol = self.clone();
        Box::pin(async move {
            protocol
                .advance_inner(context, request, published_observations)
                .await
        })
    }

    fn reset(
        &self,
        context: PublicSimulationContext,
        request: ResetRequest,
        next_timeline_id: String,
    ) -> BoundaryFuture<()> {
        let protocol = self.clone();
        Box::pin(async move {
            protocol
                .reset_inner(context, request, next_timeline_id)
                .await
        })
    }

    fn release(
        &self,
        _context: PublicSimulationContext,
        _request: ReleaseAuthorityRequest,
    ) -> BoundaryFuture<()> {
        Box::pin(async { Ok(()) })
    }

    fn progress(
        &self,
        context: PublicSimulationContext,
        _request: ProgressRequest,
    ) -> BoundaryFuture<ProgressResponse> {
        let protocol = self.clone();
        Box::pin(async move {
            let boundary = protocol.inner.boundary.lock().await;
            if context.execution_id != protocol.inner.execution_id
                || context.timeline_id != boundary.timeline_id
            {
                return Err("progress identity is stale".to_owned());
            }
            Ok(ProgressResponse {
                execution_id: protocol.inner.execution_id.clone(),
                timeline_id: boundary.timeline_id.clone(),
                completed_boundary: boundary.committed_boundary,
                failed: boundary.fault.is_some(),
                detail: boundary.fault.clone(),
                session_id: context.session_id,
                authority_grant: context.authority_grant,
                correlation_id: context.correlation_id,
            })
        })
    }
}

async fn declare(bus: &BusHandle, instance: &str, leg: &str) -> Result<Subscriber> {
    let expression = OwnedKeyExpr::new(execution_protocol::key(bus, instance, leg))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let session = bus.session()?;
    session
        .declare_subscriber(expression)
        .with(zenoh::handlers::FifoChannel::new(CONTROL_CHANNEL_CAPACITY))
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

async fn send<M: Message>(bus: &BusHandle, instance: &str, leg: &str, message: &M) -> Result<()> {
    let session = bus.session()?;
    session
        .put(
            execution_protocol::key(bus, instance, leg),
            execution_protocol::encode(message)?,
        )
        .encoding(Encoding::from(
            execution_protocol::PROTOBUF_ENCODING.to_owned(),
        ))
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(())
}

async fn recv_admission(subscriber: &Subscriber) -> Result<wire::AdmitExecutionResponse> {
    decode(subscriber.recv_async().await.map_err(|error| anyhow::anyhow!(error.to_string()))?)
}

async fn recv_ready(subscriber: &Subscriber) -> Result<wire::Ready> {
    decode(subscriber.recv_async().await.map_err(|error| anyhow::anyhow!(error.to_string()))?)
}

async fn recv_reset(subscriber: &Subscriber) -> Result<wire::ResetExecutionResponse> {
    decode(subscriber.recv_async().await.map_err(|error| anyhow::anyhow!(error.to_string()))?)
}

fn expected_deliveries(
    runtime: &RuntimeInstance,
    accepted: &wire::InvocationAccepted,
    delivery_routes: &BTreeMap<(String, String, String), BTreeSet<String>>,
    request_routes: &BTreeSet<(String, String, String)>,
) -> Result<Vec<ExpectedDelivery>, String> {
    if accepted.required_deliveries.len() > MAX_PRODUCT_RECEIPTS {
        return Err("runtime delivery receipt set is too large".to_owned());
    }
    let mut expected = BTreeSet::new();
    for receipt in &accepted.required_deliveries {
        if receipt.port.is_empty()
            || receipt.direction.is_empty()
            || receipt.direction == "completion"
        {
            return Err("runtime delivery receipt has an invalid port or direction".to_owned());
        }
        let targets = match receipt.direction.as_str() {
            "request" => {
                let target = receipt
                    .target
                    .as_deref()
                    .filter(|target| !target.is_empty())
                    .ok_or_else(|| {
                        "runtime request delivery receipt is missing its target".to_owned()
                    })?;
                if !request_routes.contains(&(
                    runtime.instance.clone(),
                    target.to_owned(),
                    receipt.port.clone(),
                )) {
                    return Err(
                        "runtime request delivery receipt is not in the admitted graph".to_owned(),
                    );
                }
                vec![target.to_owned()]
            }
            "publish" | "reply" => {
                let key = (
                    runtime.instance.clone(),
                    receipt.port.clone(),
                    receipt.direction.clone(),
                );
                let Some(graph_targets) = delivery_routes.get(&key) else {
                    // An output with no graph receiver is not a required
                    // delivery obligation for this execution.
                    continue;
                };
                if let Some(target) = receipt.target.as_deref() {
                    if target.is_empty() || !graph_targets.contains(target) {
                        return Err(
                            "runtime delivery receipt names a target outside the admitted graph"
                                .to_owned(),
                        );
                    }
                    vec![target.to_owned()]
                } else {
                    graph_targets.iter().cloned().collect()
                }
            }
            _ => return Err("runtime delivery receipt has an unknown direction".to_owned()),
        };
        for target in targets {
            let delivery = ExpectedDelivery {
                source: runtime.instance.clone(),
                target,
                port: receipt.port.clone(),
                direction: receipt.direction.clone(),
                sequence: receipt.sequence,
                item: receipt.item,
                bytes: receipt.bytes,
            };
            if !expected.insert(delivery) {
                return Err("runtime returned duplicate delivery receipt identity".to_owned());
            }
        }
    }
    Ok(expected.into_iter().collect())
}

async fn wait_delivery_acknowledgements(
    acknowledgements: &Subscriber,
    failures: &Subscriber,
    execution_id: &str,
    timeline_id: &str,
    instance: &str,
    boundary: u64,
    expected: &[ExpectedDelivery],
    timeout: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut pending = expected.iter().cloned().collect::<BTreeSet<_>>();
    while !pending.is_empty() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "runtime `{instance}` delivery acknowledgement timed out at boundary {boundary}"
            ));
        }
        let delivery = tokio::time::timeout(
            remaining,
            recv_delivery_ack(
                acknowledgements,
                failures,
                execution_id,
                timeline_id,
                instance,
                boundary,
                &pending,
            ),
        )
        .await
        .map_err(|_| {
            format!(
                "runtime `{instance}` delivery acknowledgement timed out at boundary {boundary}"
            )
        })??;
        pending.remove(&delivery);
    }
    Ok(())
}

async fn recv_delivery_ack(
    acknowledgements: &Subscriber,
    failures: &Subscriber,
    execution_id: &str,
    timeline_id: &str,
    instance: &str,
    boundary: u64,
    pending: &BTreeSet<ExpectedDelivery>,
) -> Result<ExpectedDelivery, String> {
    loop {
        tokio::select! {
            result = acknowledgements.recv_async() => {
                let acknowledgement: wire::DeliveryAck = decode(
                    result.map_err(|error| error.to_string())?
                ).map_err(|error: anyhow::Error| error.to_string())?;
                let Some(candidate) = delivery_ack_candidate(
                    &acknowledgement,
                    execution_id,
                    timeline_id,
                    instance,
                    boundary,
                    pending,
                ) else {
                    // This is a duplicate acknowledgement for a delivery
                    // already removed from the pending set, or a stale
                    // identity from another accepted cut.
                    continue;
                };
                if acknowledgement.admitted {
                    return Ok(candidate);
                }
                return Err(acknowledgement.detail.unwrap_or_else(|| {
                    format!(
                        "receiver `{}` refused delivery admission for `{}`",
                        candidate.target, candidate.port
                    )
                }));
            }
            result = failures.recv_async() => {
                let failure: wire::RuntimeFailure = decode(
                    result.map_err(|error| error.to_string())?
                ).map_err(|error: anyhow::Error| error.to_string())?;
                if failure.execution_id == execution_id
                    && failure.timeline_id == timeline_id
                    && failure.runtime_instance == instance
                    && failure.boundary == boundary
                {
                    return Err(failure.reason);
                }
            }
        }
    }
}

fn delivery_ack_candidate(
    acknowledgement: &wire::DeliveryAck,
    execution_id: &str,
    timeline_id: &str,
    instance: &str,
    boundary: u64,
    pending: &BTreeSet<ExpectedDelivery>,
) -> Option<ExpectedDelivery> {
    if acknowledgement.execution_id != execution_id
        || acknowledgement.timeline_id != timeline_id
        || acknowledgement.boundary != boundary
        || acknowledgement.source != instance
    {
        return None;
    }
    let candidate = ExpectedDelivery {
        source: acknowledgement.source.clone(),
        target: acknowledgement.target.clone(),
        port: acknowledgement.port.clone(),
        direction: acknowledgement.direction.clone(),
        sequence: acknowledgement.sequence,
        item: acknowledgement.item,
        bytes: acknowledgement.bytes,
    };
    pending.get(&candidate).cloned()
}

async fn recv_acceptance(
    accepted: &Subscriber,
    failures: &Subscriber,
    execution_id: &str,
    timeline_id: &str,
    instance: &str,
    boundary: u64,
    product_ports: &BTreeSet<String>,
) -> Result<wire::InvocationAccepted, String> {
    loop {
        tokio::select! {
            result = accepted.recv_async() => {
                let message: wire::InvocationAccepted = decode(result.map_err(|error| error.to_string())?).map_err(|error: anyhow::Error| error.to_string())?;
                if message.execution_id != execution_id || message.timeline_id != timeline_id || message.runtime_instance != instance || message.boundary != boundary {
                    // A replayed or delayed acceptance can remain in the
                    // bounded control queue after a retry.  It is evidence
                    // for another invocation, never permission to complete
                    // this one, so discard it without reinvoking the runtime.
                    continue;
                }
                validate_products(&message, product_ports)?;
                return Ok(message);
            }
            result = failures.recv_async() => {
                let failure: wire::RuntimeFailure = decode(result.map_err(|error| error.to_string())?).map_err(|error: anyhow::Error| error.to_string())?;
                if failure.execution_id == execution_id && failure.timeline_id == timeline_id && failure.runtime_instance == instance && failure.boundary == boundary {
                    return Err(failure.reason);
                }
            }
        }
    }
}

fn validate_products(
    accepted: &wire::InvocationAccepted,
    product_ports: &BTreeSet<String>,
) -> Result<(), String> {
    if accepted.required_products.len() > MAX_PRODUCT_RECEIPTS {
        return Err("runtime product receipt set is too large".to_owned());
    }
    let mut seen = BTreeSet::new();
    for receipt in &accepted.required_products {
        if receipt.port.is_empty()
            || (receipt.items == 0 && receipt.bytes != 0)
            || !product_ports.contains(&receipt.port)
            || !seen.insert(receipt.port.as_str())
        {
            return Err("runtime product receipt is malformed or not in the admitted graph".to_owned());
        }
    }
    Ok(())
}

fn validate_published_observations(
    request: &AdvanceRequest,
    published: &[crate::communication::simulation::ProductReceipt],
) -> Result<(), String> {
    if published.len() > MAX_PRODUCT_RECEIPTS || published.len() != request.observations.len() {
        return Err("published observation receipts are not complete".to_owned());
    }
    let expected = request
        .observations
        .iter()
        .map(|observation| (observation.service_instance.as_str(), observation.port.as_str()))
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for receipt in published {
        if receipt.service_instance.is_empty()
            || receipt.port.is_empty()
            || receipt.boundary != request.boundary
            || receipt.correlation_id != request.correlation_id
            || !seen.insert((receipt.service_instance.as_str(), receipt.port.as_str()))
        {
            return Err("published observation receipt has invalid identity".to_owned());
        }
    }
    if seen != expected {
        return Err("published observation receipts do not match the requested providers".to_owned());
    }
    Ok(())
}

fn validate_input_receipts(
    accepted: &wire::InvocationAccepted,
    runtime: &RuntimeInstance,
) -> Result<(), String> {
    if accepted.required_inputs.len() > MAX_PRODUCT_RECEIPTS {
        return Err("runtime input receipt set is too large".to_owned());
    }
    let mut seen = BTreeSet::new();
    for receipt in &accepted.required_inputs {
        if receipt.source.is_empty()
            || receipt.port.is_empty()
            || (receipt.items == 0 && receipt.bytes != 0)
            || !runtime.input_ports.contains(&receipt.port)
            || !runtime
                .input_sources
                .get(&receipt.port)
                .is_some_and(|sources| {
                    sources.contains(&(receipt.source.clone(), receipt.port.clone()))
                })
            || !seen.insert((receipt.source.as_str(), receipt.port.as_str()))
        {
            return Err("runtime input receipt is malformed, duplicated, or not in the admitted graph".to_owned());
        }
    }
    Ok(())
}

fn validate_actuations(
    accepted: &wire::InvocationAccepted,
    runtime: &RuntimeInstance,
    logical_time_ns: u64,
) -> Result<Vec<wire::Actuation>, String> {
    if accepted.actuations.len() > MAX_PRODUCT_RECEIPTS {
        return Err("runtime actuation set is too large".to_owned());
    }
    let mut seen = BTreeSet::new();
    for actuation in &accepted.actuations {
        let Some(max_bytes) = runtime.actuation_max_bytes.get(&actuation.port) else {
            return Err("runtime returned an actuation for a foreign port".to_owned());
        };
        if actuation.payload.is_empty()
            || actuation.payload.len() as u64 > *max_bytes
            || actuation.valid_until_ns <= logical_time_ns
            || !seen.insert(actuation.port.as_str())
        {
            return Err("runtime returned an invalid or duplicate actuation".to_owned());
        }
    }
    if seen.len() != runtime.actuation_ports.len() {
        return Err("runtime omitted a required actuation".to_owned());
    }
    Ok(accepted.actuations.clone())
}

fn decode<M: Message + Default>(sample: zenoh::sample::Sample) -> Result<M> {
    if sample.encoding().to_string() != execution_protocol::PROTOBUF_ENCODING {
        bail!("private execution response used an unexpected encoding");
    }
    Ok(execution_protocol::decode(sample.payload().to_bytes().as_ref())?)
}

fn decode_digest(value: &str) -> Option<Vec<u8>> {
    if value.len() != 64 || !value.is_ascii() {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            Some(
                ((pair[0] as char).to_digit(16)? * 16 + (pair[1] as char).to_digit(16)?) as u8,
            )
        })
        .collect()
}

fn parse_timeline_id(value: &str) -> Result<crate::identity::TimelineId, String> {
    let digits = value
        .strip_prefix('t')
        .ok_or_else(|| "timeline must use the canonical t-prefixed form".to_owned())?;
    if digits.len() != 16 || !digits.is_ascii() {
        return Err("timeline must contain exactly 16 hexadecimal digits".to_owned());
    }
    let raw = u64::from_str_radix(digits, 16)
        .map_err(|_| "timeline contains a non-hexadecimal digit".to_owned())?;
    crate::identity::TimelineId::from_raw(raw)
        .ok_or_else(|| "timeline identity cannot be zero".to_owned())
}

#[derive(Clone, Debug, serde::Deserialize)]
struct ArtifactSummary {
    runtime: ArtifactRuntime,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct ArtifactRuntime {
    #[serde(default)]
    period_ms: Option<u64>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    inputs: Vec<ArtifactInput>,
    #[serde(default)]
    transient_outputs: Vec<ArtifactOutput>,
    #[serde(default)]
    service_outputs: Vec<ArtifactOutput>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct ArtifactInput {
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    max_items: Option<u64>,
    #[serde(default)]
    max_bytes: Option<u64>,
    #[serde(default)]
    port: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct ArtifactOutput {
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    port: Option<String>,
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    max_items: Option<u64>,
    #[serde(default)]
    max_bytes: Option<u64>,
    #[serde(default)]
    max_request_bytes: Option<u64>,
    #[serde(default)]
    every_steps: Option<u64>,
    #[serde(default)]
    bootstrap: bool,
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::bus::{BusConfig, BusHandle, BusOwner};
    use crate::communication::simulation::{AdvanceRequest, ResetRequest};
    use crate::identity::{ExecutionId, ParticipantId, TimelineId};
    use crate::participant::metadata::ParticipantKind;
    use crate::runtime::ExecutionTime;
    use crate::runtime::transport::{RuntimeWireMetadata, PROTOBUF_ENCODING, port_key};
    use crate::supervisor::host::presence::Presence;
    use crate::supervisor::host::bundle::{SourceBundle, SourceExecutable, SourceManifest};

    async fn send_test_acceptance(bus: &BusHandle, invocation: &wire::Invocation, output: bool) {
        super::send(
            bus,
            &invocation.runtime_instance,
            "accepted",
            &wire::InvocationAccepted {
                execution_id: invocation.execution_id.clone(),
                timeline_id: invocation.timeline_id.clone(),
                runtime_instance: invocation.runtime_instance.clone(),
                boundary: invocation.boundary,
                required_products: if output {
                    vec![wire::ProductReceipt {
                        port: "value".to_owned(),
                        sequence: 1,
                        items: 1,
                        bytes: 1,
                    }]
                } else {
                    Vec::new()
                },
                required_inputs: Vec::new(),
                actuations: Vec::new(),
                required_deliveries: if output {
                    vec![wire::DeliveryReceipt {
                        port: "value".to_owned(),
                        direction: "publish".to_owned(),
                        target: String::new(),
                        sequence: 1,
                        item: 0,
                        bytes: 1,
                    }]
                } else {
                    Vec::new()
                },
            },
        )
        .await
        .expect("invocation acceptance publishes");
    }

    async fn publish_test_value(bus: &BusHandle, execution_id: &str, timeline_id: &str) {
        let metadata = RuntimeWireMetadata::data(
            "producer",
            ExecutionTime::from_nanos(1_000_000),
            1,
        )
        .with_delivery_identity(execution_id, timeline_id, 1, 0);
        let attachment = metadata.encode_bounded().expect("delivery metadata encodes");
        bus.session()
            .expect("producer session")
            .put(
                bus.full_key(&port_key("producer", "value", "publish")),
                vec![42_u8],
            )
            .encoding(Encoding::from(PROTOBUF_ENCODING.to_owned()))
            .attachment(attachment)
            .await
            .expect("controlled value publishes");
    }

    #[test]
    fn digest_decoding_is_strict_and_binary() {
        assert_eq!(decode_digest(&"ab".repeat(32)).expect("digest")[0], 0xab);
        assert!(decode_digest("ab").is_none());
        assert!(decode_digest(&"zz".repeat(32)).is_none());
    }

    #[test]
    fn due_boundaries_include_zero_and_only_exact_periods() {
        let quantum = 10;
        let period = 20;
        assert!(0_u64 == 0 || 0_u64.is_multiple_of(period));
        assert!(20_u64.is_multiple_of(period));
        assert!(!10_u64.is_multiple_of(period));
        assert!(quantum > 0);
    }

    #[test]
    fn delivery_ack_candidates_remove_out_of_order_items_without_loss() {
        let make = |item| ExpectedDelivery {
            source: "producer".to_owned(),
            target: "consumer".to_owned(),
            port: "value".to_owned(),
            direction: "publish".to_owned(),
            sequence: 9,
            item,
            bytes: 4,
        };
        let mut pending = BTreeSet::from([make(0), make(1), make(2)]);
        for item in [2_u32, 0, 1] {
            let acknowledgement = wire::DeliveryAck {
                execution_id: "execution".to_owned(),
                timeline_id: "timeline".to_owned(),
                boundary: 7,
                source: "producer".to_owned(),
                target: "consumer".to_owned(),
                port: "value".to_owned(),
                direction: "publish".to_owned(),
                sequence: 9,
                item,
                bytes: 4,
                admitted: true,
                detail: None,
            };
            let candidate = delivery_ack_candidate(
                &acknowledgement,
                "execution",
                "timeline",
                "producer",
                7,
                &pending,
            )
            .expect("out-of-order acknowledgement matches pending identity");
            assert!(pending.remove(&candidate));
        }
        assert!(pending.is_empty());
    }

    #[test]
    fn empty_product_and_input_batches_complete_without_new_data() {
        let accepted = wire::InvocationAccepted {
            execution_id: "execution".to_owned(),
            timeline_id: "timeline".to_owned(),
            runtime_instance: "runtime".to_owned(),
            boundary: 4,
            required_products: vec![wire::ProductReceipt {
                port: "optional".to_owned(),
                sequence: 0,
                items: 0,
                bytes: 0,
            }],
            required_inputs: Vec::new(),
            actuations: Vec::new(),
            required_deliveries: Vec::new(),
        };
        let product_ports = BTreeSet::from(["optional".to_owned()]);
        assert!(validate_products(&accepted, &product_ports).is_ok());
        assert!(validate_products(
            &wire::InvocationAccepted {
                required_products: vec![wire::ProductReceipt {
                    port: "optional".to_owned(),
                    sequence: 0,
                    items: 0,
                    bytes: 1,
                }],
                ..accepted.clone()
            },
            &product_ports,
        )
        .is_err());
    }

    #[serial_test::serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn local_router_delivery_ack_completes_before_slow_receiver_due() {
        let temporary = tempfile::tempdir().expect("router temporary directory");
        let socket = temporary.path().join("router.sock");
        let endpoint = format!("unixsock-stream/{}", socket.display());
        let execution = ExecutionId::mint();
        let router = super::super::router::start_embedded_router(
            execution,
            endpoint.clone(),
            Arc::new(|_| {}),
        )
        .await
        .expect("local router starts");
        let (supervisor_owner, supervisor_bus) = BusOwner::open(BusConfig::for_external(
            execution,
            None,
            vec![endpoint.clone()],
        ))
        .await
        .expect("supervisor bus opens");
        let (producer_owner, producer_bus) = BusOwner::open(BusConfig::for_participant(
            execution,
            ParticipantId::new("producer").expect("producer identity"),
            vec![endpoint.clone()],
        ))
        .await
        .expect("producer bus opens");
        let (consumer_owner, consumer_bus) = BusOwner::open(BusConfig::for_participant(
            execution,
            ParticipantId::new("consumer").expect("consumer identity"),
            vec![endpoint],
        ))
        .await
        .expect("consumer bus opens");
        let source = SourceBundle::for_test_with_connections(
            Path::new("."),
            SourceManifest::for_test(
                "controlled-delivery-fixture",
                vec![
                    SourceExecutable::for_test_with_artifact(
                        "producer",
                        serde_json::json!({
                            "runtime": {
                                "schema": "phoxal/artifact/v0",
                                "record": "runtime",
                                "period_ms": 1,
                                "timeout_ms": 500,
                                "inputs": [],
                                "transient_outputs": [],
                                "service_outputs": [
                                    {"name": "value", "kind": "state", "port": "value", "max_items": 1, "max_bytes": 1}
                                ]
                            },
                            "descriptors": []
                        }),
                    ),
                    SourceExecutable::for_test_with_artifact(
                        "consumer",
                        serde_json::json!({
                            "runtime": {
                                "schema": "phoxal/artifact/v0",
                                "record": "runtime",
                                "period_ms": 2,
                                "timeout_ms": 500,
                                "inputs": [
                                    {"name": "value", "kind": "latest", "port": "value", "max_items": 1, "max_bytes": 1}
                                ],
                                "transient_outputs": [],
                                "service_outputs": []
                            },
                            "descriptors": []
                        }),
                    ),
                ],
            ),
            BTreeMap::from([(
                "consumer.value".to_owned(),
                serde_json::json!("producer.value"),
            )]),
        );
        let state = ExecutionState::new(
            Presence::for_entries(vec![
                ("producer".to_owned(), ParticipantKind::Brain),
                ("consumer".to_owned(), ParticipantKind::Service),
            ])
            .expect("presence graph"),
        )
        .expect("execution state");
        let protocol = RuntimeExecutionProtocol::open(
            supervisor_bus.clone(),
            &source,
            state.clone(),
        )
        .await
        .expect("protocol opens");
        let producer_admit = super::declare(&producer_bus, "producer", "admit")
            .await
            .expect("producer admission subscriber");
        let producer_invoke = super::declare(&producer_bus, "producer", "invoke")
            .await
            .expect("producer invocation subscriber");
        let consumer_admit = super::declare(&consumer_bus, "consumer", "admit")
            .await
            .expect("consumer admission subscriber");
        let consumer_invoke = super::declare(&consumer_bus, "consumer", "invoke")
            .await
            .expect("consumer invocation subscriber");
        let value_subscriber = consumer_bus
            .session()
            .expect("consumer value session")
            .declare_subscriber(
                OwnedKeyExpr::new(consumer_bus.full_key(&port_key(
                    "producer",
                    "value",
                    "publish",
                )))
                .expect("consumer value key"),
            )
            .with(zenoh::handlers::FifoChannel::new(4))
            .await
            .expect("consumer value subscriber");

        let producer_actor_bus = producer_bus.clone();
        let producer_actor = tokio::spawn(async move {
            let admission_sample = tokio::time::timeout(
                Duration::from_secs(2),
                producer_admit.recv_async(),
            )
            .await
            .expect("producer admission arrives")
            .expect("producer admission subscriber remains open");
            let admission: wire::AdmitExecutionRequest =
                super::decode(admission_sample).expect("producer admission decodes");
            assert!(admission.required_contracts[0]
                .capabilities
                .iter()
                .any(|capability| capability == "delivery-ack"));
            super::send(
                &producer_actor_bus,
                "producer",
                "admit-response",
                &wire::AdmitExecutionResponse {
                    admitted: true,
                    unsupported_contracts: Vec::new(),
                    detail: None,
                },
            )
            .await
            .expect("producer admission response publishes");
            super::send(
                &producer_actor_bus,
                "producer",
                "ready",
                &wire::Ready {
                    execution_id: admission.execution_id.clone(),
                    timeline_id: admission.timeline_id.clone(),
                    runtime_instance: "producer".to_owned(),
                },
            )
            .await
            .expect("producer Ready publishes");
            for boundary in 0..=2_u64 {
                let invocation_sample = tokio::time::timeout(
                    Duration::from_secs(2),
                    producer_invoke.recv_async(),
                )
                .await
                .expect("producer invocation arrives")
                .expect("producer invocation subscriber remains open");
                let invocation: wire::Invocation =
                    super::decode(invocation_sample).expect("producer invocation decodes");
                assert_eq!(invocation.boundary, boundary);
                if boundary == 1 {
                    publish_test_value(
                        &producer_actor_bus,
                        &invocation.execution_id,
                        &invocation.timeline_id,
                    )
                    .await;
                }
                send_test_acceptance(&producer_actor_bus, &invocation, boundary == 1).await;
            }
        });

        let consumer_actor_bus = consumer_bus.clone();
        let consumer_actor = tokio::spawn(async move {
            let admission_sample = tokio::time::timeout(
                Duration::from_secs(2),
                consumer_admit.recv_async(),
            )
            .await
            .expect("consumer admission arrives")
            .expect("consumer admission subscriber remains open");
            let admission: wire::AdmitExecutionRequest =
                super::decode(admission_sample).expect("consumer admission decodes");
            super::send(
                &consumer_actor_bus,
                "consumer",
                "admit-response",
                &wire::AdmitExecutionResponse {
                    admitted: true,
                    unsupported_contracts: Vec::new(),
                    detail: None,
                },
            )
            .await
            .expect("consumer admission response publishes");
            super::send(
                &consumer_actor_bus,
                "consumer",
                "ready",
                &wire::Ready {
                    execution_id: admission.execution_id.clone(),
                    timeline_id: admission.timeline_id.clone(),
                    runtime_instance: "consumer".to_owned(),
                },
            )
            .await
            .expect("consumer Ready publishes");
            let mut retained_values = Vec::new();
            loop {
                tokio::select! {
                    invocation_sample = consumer_invoke.recv_async() => {
                        let invocation: wire::Invocation = super::decode(
                            invocation_sample.expect("consumer invocation subscriber remains open")
                        ).expect("consumer invocation decodes");
                        match invocation.boundary {
                            0 => send_test_acceptance(&consumer_actor_bus, &invocation, false).await,
                            2 => {
                                loop {
                                    match value_subscriber.try_recv() {
                                        Ok(Some(sample)) => retained_values.push(
                                            crate::runtime::transport::WireSample::from_zenoh(sample)
                                                .expect("retained value decodes")
                                        ),
                                        Ok(None) => break,
                                        Err(error) => panic!("consumer value subscriber failed: {error}"),
                                    }
                                }
                                assert_eq!(retained_values.len(), 1, "slow consumer sees one value at its due invocation");
                                let value = &retained_values[0];
                                assert_eq!(value.payload(), [42]);
                                assert_eq!(value.metadata().boundary, Some(1));
                                assert_eq!(value.metadata().item, Some(0));
                                assert_eq!(value.metadata().sequence, Some(1));
                                send_test_acceptance(&consumer_actor_bus, &invocation, false).await;
                                break;
                            }
                            boundary => panic!("unexpected consumer invocation boundary {boundary}"),
                        }
                    }
                    sample = value_subscriber.recv_async() => {
                        let value = crate::runtime::transport::WireSample::from_zenoh(
                            sample.expect("consumer value subscriber remains open")
                        ).expect("controlled value decodes");
                        assert_eq!(value.payload(), [42]);
                        assert_eq!(value.metadata().boundary, Some(1));
                        assert_eq!(value.metadata().item, Some(0));
                        assert_eq!(value.metadata().sequence, Some(1));
                        retained_values.push(value);
                        super::send(
                            &consumer_actor_bus,
                            "producer",
                            "delivery-ack",
                            &wire::DeliveryAck {
                                execution_id: admission.execution_id.clone(),
                                timeline_id: admission.timeline_id.clone(),
                                boundary: 1,
                                source: "producer".to_owned(),
                                target: "consumer".to_owned(),
                                port: "value".to_owned(),
                                direction: "publish".to_owned(),
                                sequence: 1,
                                item: 0,
                                bytes: 1,
                                admitted: true,
                                detail: None,
                            },
                        )
                        .await
                        .expect("delivery acknowledgement publishes");
                    }
                }
            }
        });

        let timeline = state.time_domain().timeline.to_string();
        protocol
            .admit_all(RuntimeExecutionMode::Controlled, 1_000_000, &timeline)
            .await
            .expect("controlled admission and Ready complete");
        let mut context = PublicSimulationContext {
            principal: "simulator".to_owned(),
            session_id: vec![1],
            authority_grant: vec![2],
            correlation_id: vec![3],
            execution_id: execution.to_string(),
            timeline_id: timeline.clone(),
            completed_boundary: 0,
            model_identity: "unused".to_owned(),
            quantum_ns: 1_000_000,
        };
        let mut request = AdvanceRequest {
            authority_grant: vec![2],
            execution_id: execution.to_string(),
            timeline_id: timeline,
            boundary: 0,
            observations: Vec::new(),
            session_id: vec![1],
            correlation_id: vec![3],
        };
        for boundary in 0..=2_u64 {
            context.completed_boundary = boundary;
            request.boundary = boundary;
            let response = protocol
                .advance(context.clone(), request.clone(), Vec::new())
                .await
                .expect("controlled boundary completes");
            assert_eq!(response.completed_boundary, boundary + 1);
        }
        assert_eq!(state.runtime_boundary(), 3);
        producer_actor.await.expect("producer actor completes");
        consumer_actor.await.expect("consumer actor completes");
        consumer_owner.close().await;
        producer_owner.close().await;
        supervisor_owner.close().await;
        router.close().await.expect("router closes");
    }

    #[serial_test::serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_router_control_exchange_commits_only_the_complete_cut() {
        let temporary = tempfile::tempdir().expect("router temporary directory");
        let socket = temporary.path().join("router.sock");
        let endpoint = format!("unixsock-stream/{}", socket.display());
        let execution = ExecutionId::mint();
        let router = super::super::router::start_embedded_router(
            execution,
            endpoint.clone(),
            Arc::new(|_| {}),
        )
        .await
        .expect("local router starts");
        let (supervisor_owner, supervisor_bus) = BusOwner::open(BusConfig::for_external(
            execution,
            None,
            vec![endpoint.clone()],
        ))
        .await
        .expect("supervisor bus opens");
        let (runtime_owner, runtime_bus) = BusOwner::open(BusConfig::for_participant(
            execution,
            ParticipantId::new("brain").expect("runtime identity"),
            vec![endpoint],
        ))
        .await
        .expect("runtime bus opens");
        let source = SourceBundle::for_test(
            Path::new("."),
            SourceManifest::for_test(
                "controlled-fixture",
                vec![SourceExecutable::for_test_with_artifact(
                    "brain",
                    serde_json::json!({
                        "runtime": {
                            "schema": "phoxal/artifact/v0",
                            "record": "runtime",
                            "period_ms": 1,
                            "timeout_ms": 500,
                            "inputs": [],
                            "transient_outputs": [],
                            "service_outputs": [
                                {"name": "state", "kind": "state", "port": "state", "max_bytes": 64},
                                {"name": "target", "kind": "setpoint", "port": "target", "max_bytes": 8}
                            ]
                        },
                        "descriptors": []
                    }),
                )],
            ),
        );
        let state = ExecutionState::new(
            Presence::for_entries(vec![("brain".to_owned(), ParticipantKind::Brain)])
                .expect("presence graph"),
        )
        .expect("execution state");
        let protocol = RuntimeExecutionProtocol::open(
            supervisor_bus.clone(),
            &source,
            state.clone(),
        )
        .await
        .expect("protocol opens");
        let admit = super::declare(&runtime_bus, "brain", "admit")
            .await
            .expect("runtime admission subscriber");
        let invoke = super::declare(&runtime_bus, "brain", "invoke")
            .await
            .expect("runtime invocation subscriber");
        let reset = super::declare(&runtime_bus, "brain", "reset")
            .await
            .expect("runtime reset subscriber");
        let actor_bus = runtime_bus.clone();
        let timeline = state.time_domain().timeline.to_string();
        let actor_timeline = timeline.clone();
        let actor = tokio::spawn(async move {
            let admission_sample = tokio::time::timeout(Duration::from_secs(2), admit.recv_async())
                .await
                .expect("admission request arrives")
                .expect("admission subscriber remains open");
            let admission: wire::AdmitExecutionRequest =
                super::decode(admission_sample).expect("admission decodes");
            assert_eq!(admission.mode, wire::ExecutionMode::Controlled as i32);
            assert_eq!(admission.quantum_ns, 1_000_000);
            super::send(
                &actor_bus,
                "brain",
                "admit-response",
                &wire::AdmitExecutionResponse {
                    admitted: true,
                    unsupported_contracts: Vec::new(),
                    detail: None,
                },
            )
            .await
            .expect("admission response publishes");
            super::send(
                &actor_bus,
                "brain",
                "ready",
                &wire::Ready {
                    execution_id: admission.execution_id.clone(),
                    timeline_id: admission.timeline_id.clone(),
                    runtime_instance: "brain".to_owned(),
                },
            )
            .await
            .expect("ready response publishes");
            let invocation_sample = tokio::time::timeout(Duration::from_secs(2), invoke.recv_async())
                .await
                .expect("invocation arrives")
                .expect("invocation subscriber remains open");
            let invocation: wire::Invocation =
                super::decode(invocation_sample).expect("invocation decodes");
            assert_eq!(invocation.boundary, 0);
            assert_eq!(invocation.logical_time_ns, 0);
            super::send(
                &actor_bus,
                "brain",
                "accepted",
                &wire::InvocationAccepted {
                    execution_id: invocation.execution_id.clone(),
                    timeline_id: invocation.timeline_id.clone(),
                    runtime_instance: invocation.runtime_instance.clone(),
                    boundary: invocation.boundary,
                    required_products: vec![wire::ProductReceipt {
                        port: "state".to_owned(),
                        sequence: 1,
                        items: 1,
                        bytes: 1,
                    }],
                    required_inputs: Vec::new(),
                    actuations: vec![wire::Actuation {
                        port: "target".to_owned(),
                        payload: vec![7],
                        valid_until_ns: 1_000_001,
                    }],
                    required_deliveries: Vec::new(),
                },
            )
            .await
            .expect("acceptance publishes");
            let reset_sample = tokio::time::timeout(Duration::from_secs(2), reset.recv_async())
                .await
                .expect("reset request arrives")
                .expect("reset subscriber remains open");
            let request: wire::ResetExecutionRequest =
                super::decode(reset_sample).expect("reset decodes");
            assert_eq!(request.retired_timeline_id, actor_timeline);
            super::send(
                &actor_bus,
                "brain",
                "reset-response",
                &wire::ResetExecutionResponse {
                    accepted: true,
                    detail: None,
                },
            )
            .await
            .expect("reset response publishes");
        });
        protocol
            .admit_all(RuntimeExecutionMode::Controlled, 1_000_000, &timeline)
            .await
            .expect("admission and private Ready identity validation complete");
        let context = PublicSimulationContext {
            principal: "simulator".to_owned(),
            session_id: vec![1],
            authority_grant: vec![2],
            correlation_id: vec![3],
            execution_id: execution.to_string(),
            timeline_id: timeline.clone(),
            completed_boundary: 0,
            model_identity: "unused".to_owned(),
            quantum_ns: 1_000_000,
        };
        let request = AdvanceRequest {
            authority_grant: vec![2],
            execution_id: execution.to_string(),
            timeline_id: timeline.clone(),
            boundary: 0,
            observations: Vec::new(),
            session_id: vec![1],
            correlation_id: vec![3],
        };
        let response = protocol
            .advance(context.clone(), request.clone(), Vec::new())
            .await
            .expect("complete runtime cut commits");
        assert_eq!(response.completed_boundary, 1);
        assert_eq!(response.actuation.len(), 1);
        assert_eq!(response.actuation[0].service_instance, "brain");
        assert_eq!(response.actuation[0].payload, vec![7]);
        assert_eq!(state.runtime_boundary(), 1);
        let stale = protocol.advance(context, request, Vec::new()).await;
        assert!(stale.is_err(), "duplicate old boundary cannot reinvoke");
        assert_eq!(state.runtime_boundary(), 1, "stale request cannot fabricate progress");
        let next_timeline = TimelineId::mint().to_string();
        protocol
            .reset(
                PublicSimulationContext {
                    principal: "simulator".to_owned(),
                    session_id: vec![1],
                    authority_grant: vec![2],
                    correlation_id: vec![4],
                    execution_id: execution.to_string(),
                    timeline_id: timeline.clone(),
                    completed_boundary: 1,
                    model_identity: "unused".to_owned(),
                    quantum_ns: 1_000_000,
                },
                ResetRequest {
                    authority_grant: vec![2],
                    execution_id: execution.to_string(),
                    timeline_id: timeline,
                    completed_boundary: 1,
                    session_id: vec![1],
                    correlation_id: vec![4],
                },
                next_timeline.clone(),
            )
            .await
            .expect("reset response commits a fresh fence");
        assert_eq!(state.runtime_boundary(), 0);
        assert!(
            protocol
                .progress(
                    PublicSimulationContext {
                        principal: "simulator".to_owned(),
                        session_id: vec![1],
                        authority_grant: vec![2],
                        correlation_id: vec![5],
                        execution_id: execution.to_string(),
                        timeline_id: next_timeline,
                        completed_boundary: 0,
                        model_identity: "unused".to_owned(),
                        quantum_ns: 1_000_000,
                    },
                    ProgressRequest::default(),
                )
                .await
                .is_ok()
        );
        actor.await.expect("runtime actor completes");
        runtime_owner.close().await;
        supervisor_owner.close().await;
        router.close().await.expect("router closes");
    }
}
