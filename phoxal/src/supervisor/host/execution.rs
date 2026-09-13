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
        for executable in source.executables() {
            let instance = executable.instance().to_owned();
            let digest = decode_digest(executable.sha256())
                .with_context(|| format!("runtime `{instance}` has an invalid executable digest"))?;
            let artifact = source
                .artifact(&instance)
                .with_context(|| format!("runtime `{instance}` has no artifact contract"))?;
            let artifact: ArtifactSummary = serde_json::from_value(artifact.clone())
                .with_context(|| format!("runtime `{instance}` artifact contract is invalid"))?;
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
            let product_ports = artifact
                .runtime
                .transient_outputs
                .iter()
                .chain(artifact.runtime.service_outputs.iter())
                .filter(|output| {
                    matches!(
                        output.kind.as_str(),
                        "state" | "sample" | "event" | "stream" | "read"
                    )
                })
                .map(|output| output.port.clone().unwrap_or_else(|| output.name.clone()))
                .collect::<BTreeSet<_>>();
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
            });
        }
        Ok(Self {
            inner: Arc::new(ProtocolInner {
                execution_id: bus.execution().to_string(),
                bus,
                state: state.clone(),
                instances,
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
                capabilities: vec!["invocation".to_owned(), "reset".to_owned()],
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
            actuations.extend(accepted_actuations.into_iter().map(|actuation| {
                crate::communication::simulation::Actuation {
                    service_instance: runtime.instance.clone(),
                    port: actuation.port,
                    payload: actuation.payload,
                    valid_until_ns: actuation.valid_until_ns,
                }
            }));
        }
        // Input receipts prove what each due runtime froze, but they do not
        // prove receiver queue admission for a provider or graph product.
        // That requires a separate route acknowledgement leg and is kept out
        // of this protocol until the receiver can acknowledge the exact
        // source/port/sequence/boundary without coupling it to due selection.
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
            || receipt.items == 0
            || !product_ports.contains(&receipt.port)
            || !seen.insert(receipt.port.as_str())
        {
            return Err("runtime product receipt is malformed or not in the admitted graph".to_owned());
        }
    }
    let expected = product_ports.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if seen != expected {
        return Err("runtime did not return the complete admitted product receipt set".to_owned());
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
            || receipt.items == 0
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

#[derive(Debug, serde::Deserialize)]
struct ArtifactSummary {
    runtime: ArtifactRuntime,
}

#[derive(Debug, serde::Deserialize)]
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

#[derive(Debug, serde::Deserialize)]
struct ArtifactInput {
    name: String,
    #[serde(default)]
    port: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ArtifactOutput {
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    port: Option<String>,
    #[serde(default)]
    max_bytes: Option<u64>,
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::bus::{BusConfig, BusOwner};
    use crate::communication::simulation::{AdvanceRequest, ResetRequest};
    use crate::identity::{ExecutionId, ParticipantId, TimelineId};
    use crate::participant::metadata::ParticipantKind;
    use crate::supervisor::host::presence::Presence;
    use crate::supervisor::host::bundle::{SourceBundle, SourceExecutable, SourceManifest};

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
