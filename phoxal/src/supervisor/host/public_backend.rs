//! Supervisor-owned bridges for the typed public Runtime surface.
//!
//! The public session protocol deliberately knows nothing about a service's
//! Rust types.  This module therefore forwards the already-encoded generated
//! Protobuf body to the exact Runtime port selected by the admitted bundle
//! graph, and maps Runtime wire metadata back into the public observation
//! record.  The bundle remains the only source of public port identity and
//! bounds.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use prost::Message;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, watch};
use zenoh::bytes::Encoding;
use zenoh::key_expr::OwnedKeyExpr;

use crate::bus::BusHandle;
use crate::communication::{ExecutionDefinition, PublicOperation, ServicePorts, SimulationDefinition};
use crate::communication::session::{
    PortKind, PortMetadata, RecordKind, SubscriptionRecord, SubscriptionRequest,
};
use crate::communication::simulation::{
    AcquireAuthorityRequest, AdvanceRequest, AdvanceResponse, ProgressRequest, ProgressResponse,
    ReleaseAuthorityRequest, ResetRequest,
};
use crate::communication_transport::{
    PublicBackendError, PublicBackendOutcome, PublicBackendSubscription, PublicBindingContext,
    PublicSessionBackend, PublicSimulationBackend, PublicSimulationContext,
};
use crate::runtime::{ExecutionTime, ObservationStamp};
use crate::runtime::transport::{
    PROTOBUF_ENCODING, RuntimeWireMetadata, WireControl, WireSample, port_key,
};

use super::bundle::{Bundle, SourceBundle};

const PUBLIC_INGRESS_INSTANCE: &str = "supervisor";
const PUBLIC_INGRESS_FIELD: &str = "public";
const MAX_RUNTIME_SUBSCRIBER_ITEMS: usize = 4_096;
const MAX_RUNTIME_METADATA_BYTES: usize = 1_024;
const MAX_RETAINED_ADVANCES: usize = 256;

/// The exact runtime-facing facts for one admitted public port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimePortContract {
    /// Public metadata returned by `ListPorts` and `Bind`.
    pub(crate) metadata: PortMetadata,
    /// Maximum encoded request body accepted by the generated runtime.
    request_max_bytes: u64,
    /// Maximum encoded response/publication body accepted by the generated runtime.
    response_max_bytes: u64,
}

/// The ordering facts assigned by the supervisor/coordinator to one external
/// request.  The sequence is distinct from the request correlation so a
/// runtime can preserve ingress order even when callers use arbitrary
/// operation identifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExternalIngressTicket {
    /// The first boundary at which this request may enter an input cut.
    pub(crate) eligible_boundary: u64,
    /// Monotonic order of external requests at that boundary.
    pub(crate) ingress_sequence: u64,
}

/// Runtime-owned admission of an external public call.
///
/// The supervisor resolves the reserved caller identity from the immutable
/// compiled surface, then asks this hook for the current boundary and ingress
/// sequence.  The hook is intentionally required for production calls: a
/// host-local counter or a guessed boundary would violate controlled-before-
/// external ordering after the Runtime advances.
pub(crate) trait RuntimeExternalIngress: Send + Sync {
    fn admit(
        &self,
        target_instance: &str,
        target_port: &str,
        caller: &RuntimeIngressIdentity,
        contract: &RuntimePortContract,
    ) -> Result<ExternalIngressTicket, PublicBackendError>;
}

/// Explicit absence of the Runtime external-admission seam.
///
/// This is used only while the selected Runtime has no implementation of the
/// external boundary contract.  It refuses before Zenoh admission rather than
/// inventing boundary zero or silently dropping ingress sequencing.
#[derive(Debug, Default)]
pub(crate) struct NoExternalIngress;

impl RuntimeExternalIngress for NoExternalIngress {
    fn admit(
        &self,
        target_instance: &str,
        target_port: &str,
        _caller: &RuntimeIngressIdentity,
        _contract: &RuntimePortContract,
    ) -> Result<ExternalIngressTicket, PublicBackendError> {
        Err(PublicBackendError::RejectedBeforeAdmission(format!(
            "Runtime `{target_instance}.{target_port}` has no external ingress admission hook"
        )))
    }
}

/// The source/caller identity used for supervisor-originated Runtime ingress.
///
/// A runtime must admit this identity through its compiled execution contract.
/// The host never impersonates an authored graph caller, and never omits the
/// caller metadata merely because this request originated locally.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeIngressIdentity {
    pub(crate) source: String,
    pub(crate) caller: String,
}

impl Default for RuntimeIngressIdentity {
    fn default() -> Self {
        Self {
            source: PUBLIC_INGRESS_INSTANCE.to_owned(),
            caller: format!("{PUBLIC_INGRESS_INSTANCE}.{PUBLIC_INGRESS_FIELD}"),
        }
    }
}

/// The generated public inventory and runtime bridge for one bundle.
#[derive(Clone)]
pub(crate) struct RuntimePublicSurface {
    /// Exact service/driver inventory used by the public adapter.
    pub(crate) services: Vec<ServicePorts>,
    /// Exact port contracts used by the transport bridge.
    pub(crate) ports: Arc<BTreeMap<(String, String), RuntimePortContract>>,
    /// Explicit supervisor external caller identity.
    pub(crate) ingress: RuntimeIngressIdentity,
}

impl std::fmt::Debug for RuntimePublicSurface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimePublicSurface")
            .field("services", &self.services.len())
            .field("ports", &self.ports.len())
            .field("ingress", &self.ingress)
            .finish()
    }
}

impl RuntimePublicSurface {
    /// Extract exact public ports from the source bundle's retained artifact
    /// summaries.  No public metadata is synthesized from implementation
    /// names or from an empty service placeholder.
    pub(crate) fn from_bundle(bundle: &Bundle) -> Result<Self> {
        let Bundle::Source(source) = bundle else {
            return Ok(Self {
                services: Vec::new(),
                ports: Arc::new(BTreeMap::new()),
                ingress: RuntimeIngressIdentity::default(),
            });
        };
        Self::from_source(source)
    }

    fn from_source(source: &SourceBundle) -> Result<Self> {
        let ingress = RuntimeIngressIdentity::default();
        let mut services = Vec::new();
        let mut ports = BTreeMap::new();
        // The root-local brain is an ordinary Runtime in the graph.  If it
        // owns a generated public port, that exact artifact contract belongs
        // in the same inventory as a service or driver; role is not a public
        // visibility boundary.
        for executable in source.executables() {
            let instance = executable.instance().to_owned();
            let artifact = source.artifact(&instance).ok_or_else(|| {
                anyhow::anyhow!(
                    "source executable `{instance}` has no retained Runtime artifact contract"
                )
            })?;
            let artifact: ArtifactSummary = serde_json::from_value(artifact.clone())
                .with_context(|| format!("invalid Runtime artifact contract for `{instance}`"))?;
            let mut service_ports = Vec::new();
            let mut runtime_ports = BTreeMap::new();

            for output in artifact
                .runtime
                .transient_outputs
                .iter()
                .chain(artifact.runtime.service_outputs.iter())
            {
                let Some(port) = output.port.as_deref() else {
                    continue;
                };
                let Some(signature) = output.signature.as_ref() else {
                    bail!(
                        "Runtime output `{instance}.{}` has a public port without a signature",
                        output.name
                    );
                };
                let kind = output_kind(&output.kind).with_context(|| {
                    format!("Runtime output `{instance}.{}` has an invalid public kind", output.name)
                })?;
                if kind == PortKind::Commands {
                    bail!("Runtime output `{instance}.{}` cannot serve Commands", output.name);
                }
                validate_signature(signature, port, kind)?;
                let response_max_bytes = positive_bound(
                    output.max_bytes,
                    &format!("Runtime output `{instance}.{port}` response bytes"),
                )?;
                let max_buffered_items = output
                    .max_items
                    .or_else(|| singular_public_item_bound(kind))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Runtime output `{instance}.{port}` has no bounded item count"
                        )
                    })?;
                let max_buffered_items = bounded_u32(
                    max_buffered_items,
                    &format!("Runtime output `{instance}.{port}` item count"),
                )?;
                let metadata = PortMetadata {
                    name: port.to_owned(),
                    kind: kind as i32,
                    input_fqn: signature.request.clone(),
                    output_fqn: signature.response.clone(),
                    max_message_bytes: bounded_u32(
                        response_max_bytes,
                        &format!("Runtime output `{instance}.{port}` response bytes"),
                    )?,
                    max_buffered_items,
                };
                let contract = RuntimePortContract {
                    metadata: metadata.clone(),
                    request_max_bytes: output.max_request_bytes.unwrap_or(response_max_bytes),
                    response_max_bytes,
                };
                insert_runtime_port(
                    &mut runtime_ports,
                    &mut service_ports,
                    port,
                    contract,
                )?;
            }

            for input in &artifact.runtime.inputs {
                if input.kind != "commands" {
                    continue;
                }
                let port = input.port.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Commands input `{instance}.{}` has no public port",
                        input.name
                    )
                })?;
                let signature = input.signature.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Commands input `{instance}.{port}` has no generated signature"
                    )
                })?;
                validate_signature(signature, port, PortKind::Commands)?;
                let request_max_bytes = positive_bound(
                    input.max_bytes,
                    &format!("Commands input `{instance}.{port}` request bytes"),
                )?;
                let max_buffered_items = bounded_u32(
                    positive_bound(
                        input.max_items,
                        &format!("Commands input `{instance}.{port}` item count"),
                    )?,
                    &format!("Commands input `{instance}.{port}` item count"),
                )?;
                let reply = artifact
                    .runtime
                    .service_outputs
                    .iter()
                    .filter(|output| {
                        output.kind == "reply" && output.input.as_deref() == Some(input.name.as_str())
                    })
                    .collect::<Vec<_>>();
                let reply = match reply.as_slice() {
                    [reply] => reply,
                    [] => {
                        bail!(
                            "Commands input `{instance}.{port}` has no generated reply bound"
                        );
                    }
                    _ => {
                        bail!(
                            "Commands input `{instance}.{port}` has multiple generated replies"
                        );
                    }
                };
                let response_max_bytes = positive_bound(
                    reply.max_bytes,
                    &format!("Commands input `{instance}.{port}` response bytes"),
                )?;
                let metadata = PortMetadata {
                    name: port.to_owned(),
                    kind: PortKind::Commands as i32,
                    input_fqn: signature.request.clone(),
                    output_fqn: signature.response.clone(),
                    max_message_bytes: bounded_u32(
                        response_max_bytes,
                        &format!("Commands input `{instance}.{port}` response bytes"),
                    )?,
                    max_buffered_items,
                };
                let contract = RuntimePortContract {
                    metadata: metadata.clone(),
                    request_max_bytes,
                    response_max_bytes,
                };
                insert_runtime_port(
                    &mut runtime_ports,
                    &mut service_ports,
                    port,
                    contract,
                )?;
            }

            service_ports.sort_by(|left, right| left.name.cmp(&right.name));
            let service = ServicePorts::new(instance.clone(), service_ports)?;
            for (port, contract) in runtime_ports {
                let key = (instance.clone(), port);
                if ports.insert(key.clone(), contract).is_some() {
                    bail!(
                        "source bundle contains duplicate public Runtime port `{}.{}`",
                        key.0,
                        key.1
                    );
                }
            }
            services.push(service);
        }
        services.sort_by(|left, right| left.instance().cmp(right.instance()));
        Ok(Self {
            services,
            ports: Arc::new(ports),
            ingress,
        })
    }

    /// Build the public execution definition from the exact extracted graph.
    pub(crate) fn execution(
        &self,
        execution_id: String,
        timeline_id: String,
        state: i32,
    ) -> Result<ExecutionDefinition> {
        ExecutionDefinition::new(
            crate::communication::session::ExecutionSummary {
                execution_id,
                timeline_id,
                state,
            },
            self.services.clone(),
        )
        .map_err(Into::into)
    }
}

fn insert_runtime_port(
    runtime_ports: &mut BTreeMap<String, RuntimePortContract>,
    service_ports: &mut Vec<PortMetadata>,
    port: &str,
    contract: RuntimePortContract,
) -> Result<()> {
    if runtime_ports.insert(port.to_owned(), contract.clone()).is_some() {
        bail!("compiled Runtime graph serves duplicate public port `{port}`");
    }
    service_ports.push(contract.metadata);
    Ok(())
}

fn output_kind(value: &str) -> Option<PortKind> {
    Some(match value {
        "state" => PortKind::State,
        "sample" => PortKind::Sample,
        "event" => PortKind::Event,
        "stream" => PortKind::Stream,
        "setpoint" => PortKind::Setpoint,
        "read" => PortKind::Read,
        _ => return None,
    })
}

fn singular_public_item_bound(kind: PortKind) -> Option<u64> {
    match kind {
        PortKind::State | PortKind::Setpoint | PortKind::Read => Some(1),
        PortKind::Sample | PortKind::Event | PortKind::Stream | PortKind::Commands
        | PortKind::Unspecified => None,
    }
}

fn positive_bound(value: Option<u64>, label: &str) -> Result<u64> {
    let value = value.ok_or_else(|| anyhow::anyhow!("{label} is missing"))?;
    if value == 0 {
        bail!("{label} must be positive");
    }
    Ok(value)
}

fn bounded_u32(value: u64, label: &str) -> Result<u32> {
    u32::try_from(value).with_context(|| format!("{label} exceeds the public u32 bound"))
}

fn validate_signature(signature: &ArtifactSignature, port: &str, expected: PortKind) -> Result<()> {
    if signature.name != port
        || signature.kind != artifact_kind(expected)
        || signature.service.is_empty()
        || signature.method.is_empty()
    {
        bail!(
            "generated Runtime signature for `{port}` does not match its compiled public kind"
        );
    }
    if signature.request.is_empty() || signature.response.is_empty() {
        bail!("generated Runtime signature for `{port}` has an empty message identity");
    }
    Ok(())
}

fn artifact_kind(kind: PortKind) -> &'static str {
    match kind {
        PortKind::State => "state",
        PortKind::Sample => "sample",
        PortKind::Event => "event",
        PortKind::Stream => "stream",
        PortKind::Setpoint => "setpoint",
        PortKind::Read => "read",
        PortKind::Commands => "commands",
        PortKind::Unspecified => "unspecified",
    }
}

/// A production service bridge over the supervisor's internal Runtime bus.
#[derive(Clone)]
pub(crate) struct RuntimePublicBackend {
    bus: BusHandle,
    ports: Arc<BTreeMap<(String, String), RuntimePortContract>>,
    ingress: RuntimeIngressIdentity,
    external_ingress: Arc<dyn RuntimeExternalIngress>,
    next_command: Arc<AtomicU64>,
}

impl std::fmt::Debug for RuntimePublicBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimePublicBackend")
            .field("ports", &self.ports.len())
            .field("ingress", &self.ingress)
            .finish_non_exhaustive()
    }
}

impl RuntimePublicBackend {
    pub(crate) fn new(
        bus: BusHandle,
        surface: &RuntimePublicSurface,
        external_ingress: Arc<dyn RuntimeExternalIngress>,
    ) -> Self {
        Self {
            bus,
            ports: surface.ports.clone(),
            ingress: surface.ingress.clone(),
            external_ingress,
            next_command: Arc::new(AtomicU64::new(1)),
        }
    }

    async fn call_inner(
        &self,
        operation: PublicOperation,
        binding: PublicBindingContext,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<PublicBackendOutcome, PublicBackendError> {
        let key = (binding.service_instance.clone(), binding.metadata.name.clone());
        let contract = self.ports.get(&key).ok_or_else(|| {
            PublicBackendError::RejectedBeforeAdmission(format!(
                "public Runtime port `{}.{}` is not present in the admitted bundle",
                key.0, key.1
            ))
        })?;
        let expected_kind = match operation {
            PublicOperation::Read => PortKind::Read,
            PublicOperation::Command => PortKind::Commands,
            _ => {
                return Err(PublicBackendError::RejectedBeforeAdmission(
                    "Runtime backend received a non-unary public operation".to_owned(),
                ));
            }
        };
        if binding.metadata.kind != expected_kind as i32 || binding.metadata != contract.metadata {
            return Err(PublicBackendError::RejectedBeforeAdmission(
                "public Runtime binding metadata does not match the compiled artifact".to_owned(),
            ));
        }
        if payload.len() as u64 > contract.request_max_bytes {
            return Err(PublicBackendError::RejectedBeforeAdmission(
                "public Runtime request exceeds its compiled request-byte bound".to_owned(),
            ));
        }
        let ticket = self.external_ingress.admit(
            &key.0,
            &key.1,
            &self.ingress,
            contract,
        )?;
        if ticket.ingress_sequence == 0 {
            return Err(PublicBackendError::RejectedBeforeAdmission(
                "Runtime external ingress sequence must be positive".to_owned(),
            ));
        }
        let command_id = self.next_command.fetch_add(1, Ordering::Relaxed);
        if command_id == 0 {
            return Err(PublicBackendError::RejectedBeforeAdmission(
                "public Runtime command correlation exhausted".to_owned(),
            ));
        }
        // This temporary call shape is the only base-branch gap.  Once the
        // Runtime external-admission API lands, replace it with
        // `RuntimeWireMetadata::external_command(logical_time, command_id,
        // ticket.eligible_boundary, ticket.ingress_sequence)`, which carries
        // no controlled caller rank and preserves the coordinator sequence.
        let metadata = RuntimeWireMetadata::command(
            self.ingress.source.clone(),
            ExecutionTime::from_nanos(0),
            command_id,
            ticket.eligible_boundary,
            0,
        )
        .with_caller(self.ingress.caller.clone());
        // `sequence` is the Runtime wire's producer sequence.  For an
        // external request the supervisor is that producer, so retain the
        // coordinator's ingress sequence separately from command_id.
        let metadata = RuntimeWireMetadata {
            sequence: Some(ticket.ingress_sequence),
            ..metadata
        };
        let attachment = encode_runtime_metadata(&metadata)?;
        let session = self
            .bus
            .session()
            .map_err(|error| PublicBackendError::Transport(error.to_string()))?;
        let reply_key = self
            .bus
            .full_key(&port_key(&binding.service_instance, &binding.metadata.name, "reply"));
        let subscriber = session
            .declare_subscriber(OwnedKeyExpr::new(reply_key.clone()).map_err(|error| {
                PublicBackendError::Transport(format!("invalid Runtime reply key: {error}"))
            })?)
            .with(zenoh::handlers::FifoChannel::new(8))
            .await
            .map_err(|error| PublicBackendError::Transport(error.to_string()))?;
        let request_key = self
            .bus
            .full_key(&port_key(&binding.service_instance, &binding.metadata.name, "request"));
        if let Err(error) = session
            .put(request_key, payload)
            .encoding(Encoding::from(PROTOBUF_ENCODING.to_owned()))
            .attachment(attachment)
            .await
        {
            return Ok(PublicBackendOutcome::OutcomeUnknown(format!(
                "Runtime request admission became uncertain: {error}"
            )));
        }
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout.max(Duration::from_millis(1)))
            .ok_or_else(|| PublicBackendError::Transport("Runtime deadline overflowed".to_owned()))?;
        loop {
            let sample = match tokio::time::timeout_at(deadline, subscriber.recv_async()).await {
                Ok(Ok(sample)) => sample,
                Ok(Err(error)) => {
                    return Ok(PublicBackendOutcome::OutcomeUnknown(format!(
                        "Runtime reply transport failed: {error}"
                    )));
                }
                Err(_) => {
                    return Ok(PublicBackendOutcome::OutcomeUnknown(
                        "Runtime reply deadline elapsed after request admission".to_owned(),
                    ));
                }
            };
            let wire = match WireSample::from_zenoh(sample) {
                Ok(wire) => wire,
                Err(_) => continue,
            };
            if wire.metadata().command_id != Some(command_id) {
                continue;
            }
            if wire.key() != reply_key
                || wire.metadata().source.as_deref() != Some(&key.0)
                || wire.metadata().eligible_boundary != Some(ticket.eligible_boundary)
            {
                return Ok(PublicBackendOutcome::OutcomeUnknown(
                    "Runtime reply identity did not match the admitted target".to_owned(),
                ));
            }
            if !matches!(wire.metadata().wire_control(), Ok(WireControl::Data))
                || wire.payload().len() as u64 > contract.response_max_bytes
            {
                return Ok(PublicBackendOutcome::OutcomeUnknown(
                    "Runtime reply violated its compiled transport contract".to_owned(),
                ));
            }
            return Ok(PublicBackendOutcome::Received(wire.payload().to_vec()));
        }
    }
}

impl PublicSessionBackend for RuntimePublicBackend {
    fn call(
        &self,
        operation: PublicOperation,
        binding: PublicBindingContext,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<PublicBackendOutcome, PublicBackendError>> + Send>> {
        let backend = self.clone();
        Box::pin(async move { backend.call_inner(operation, binding, payload, timeout).await })
    }

    fn subscribe(
        &self,
        operation: PublicOperation,
        binding: PublicBindingContext,
        request: SubscriptionRequest,
        capacity: usize,
    ) -> Result<PublicBackendSubscription, PublicBackendError> {
        let expected_kind = match operation {
            PublicOperation::Watch => PortKind::State,
            PublicOperation::Subscribe => match PortKind::try_from(binding.metadata.kind) {
                Ok(kind @ (PortKind::Sample | PortKind::Event | PortKind::Stream)) => kind,
                _ => {
                    return Err(PublicBackendError::RejectedBeforeAdmission(
                        "public Runtime subscription kind is not observable".to_owned(),
                    ));
                }
            },
            _ => {
                return Err(PublicBackendError::RejectedBeforeAdmission(
                    "Runtime backend received a non-subscription operation".to_owned(),
                ));
            }
        };
        let key = (binding.service_instance.clone(), binding.metadata.name.clone());
        let contract = self.ports.get(&key).ok_or_else(|| {
            PublicBackendError::RejectedBeforeAdmission(
                "public Runtime subscription port is absent from the bundle".to_owned(),
            )
        })?;
        if binding.metadata.kind != expected_kind as i32 || binding.metadata != contract.metadata {
            return Err(PublicBackendError::RejectedBeforeAdmission(
                "public Runtime subscription metadata does not match the bundle".to_owned(),
            ));
        }
        let capacity = capacity.clamp(1, MAX_RUNTIME_SUBSCRIBER_ITEMS);
        let (sender, records) = mpsc::channel(capacity);
        let bus = self.bus.clone();
        let binding = binding.clone();
        let contract = contract.clone();
        let spawn = tokio::runtime::Handle::try_current().map_err(|_| {
            PublicBackendError::Transport("public Runtime subscription has no async runtime".to_owned())
        })?;
        spawn.spawn(async move {
            let session = match bus.session() {
                Ok(session) => session,
                Err(error) => {
                    let _ = sender
                        .send(Err(PublicBackendError::Transport(error.to_string())))
                        .await;
                    return;
                }
            };
            let relative = port_key(&binding.service_instance, &binding.metadata.name, "publish");
            let key = bus.full_key(&relative);
            let subscriber = match OwnedKeyExpr::new(key.clone()) {
                Ok(key) => match session
                    .declare_subscriber(key)
                    .with(zenoh::handlers::FifoChannel::new(capacity))
                    .await
                {
                    Ok(subscriber) => subscriber,
                    Err(error) => {
                        let _ = sender
                            .send(Err(PublicBackendError::Transport(error.to_string())))
                            .await;
                        return;
                    }
                },
                Err(error) => {
                    let _ = sender
                        .send(Err(PublicBackendError::Transport(format!(
                            "invalid Runtime publication key: {error}"
                        ))))
                        .await;
                    return;
                }
            };
            loop {
                let sample = match subscriber.recv_async().await {
                    Ok(sample) => sample,
                    Err(error) => {
                        let _ = sender
                            .send(Err(PublicBackendError::Transport(error.to_string())))
                            .await;
                        return;
                    }
                };
                let record = match runtime_record(
                    sample,
                    &contract,
                    &request,
                ) {
                    Ok(record) => record,
                    Err(error) => {
                        let _ = sender.send(Err(error)).await;
                        return;
                    }
                };
                if sender.send(Ok(record)).await.is_err() {
                    return;
                }
            }
        });
        Ok(PublicBackendSubscription::new(None, records))
    }
}

fn runtime_record(
    sample: zenoh::sample::Sample,
    contract: &RuntimePortContract,
    request: &SubscriptionRequest,
) -> Result<SubscriptionRecord, PublicBackendError> {
    let wire = WireSample::from_zenoh(sample).map_err(|error| {
        PublicBackendError::Transport(format!("Runtime observation metadata is invalid: {error}"))
    })?;
    let control = wire.metadata().wire_control().map_err(|error| {
        PublicBackendError::Transport(format!("Runtime observation control is invalid: {error}"))
    })?;
    let kind = match control {
        WireControl::Data => {
            if wire.payload().len() as u64 > contract.response_max_bytes {
                return Err(PublicBackendError::Transport(
                    "Runtime observation exceeds its compiled byte bound".to_owned(),
                ));
            }
            RecordKind::Value
        }
        WireControl::Gap => RecordKind::Gap,
        WireControl::End => RecordKind::End,
        WireControl::Failed => RecordKind::Failed,
        WireControl::Rejected => {
            return Err(PublicBackendError::Transport(
                "Runtime publication used request-only rejection control".to_owned(),
            ));
        }
    };
    let payload = if control == WireControl::Data {
        wire.payload().to_vec()
    } else {
        if !wire.payload().is_empty() {
            return Err(PublicBackendError::Transport(
                "Runtime control observation carried a payload".to_owned(),
            ));
        }
        Vec::new()
    };
    Ok(SubscriptionRecord {
        session_id: request.session_id.clone(),
        binding_id: request.binding_id.clone(),
        subscription_id: request.subscription_id.clone(),
        execution_id: request.execution_id.clone(),
        timeline_id: request.timeline_id.clone(),
        revision: wire
            .metadata()
            .revision
            .or(wire.metadata().sequence)
            .unwrap_or_default(),
        kind: kind as i32,
        payload,
        dropped: 0,
        detail: if control == WireControl::Gap {
            Some("Runtime source reported a bounded observation gap".to_owned())
        } else {
            None
        },
    })
}

fn encode_runtime_metadata(metadata: &RuntimeWireMetadata) -> Result<Vec<u8>, PublicBackendError> {
    if metadata.encoded_len() > MAX_RUNTIME_METADATA_BYTES {
        return Err(PublicBackendError::RejectedBeforeAdmission(
            "Runtime metadata exceeds its bounded attachment size".to_owned(),
        ));
    }
    let mut encoded = Vec::with_capacity(metadata.encoded_len());
    metadata
        .encode(&mut encoded)
        .map_err(|error| PublicBackendError::Transport(error.to_string()))?;
    Ok(encoded)
}

/// Controlled Runtime boundary supplied by the runtime-semantics owner.
///
/// The supervisor performs public/session/lease fencing and observation
/// admission.  This hook is the only authority allowed to advance a runtime
/// boundary.  Keeping it explicit prevents a public bridge from claiming
/// completion after merely publishing sensor bytes.
pub(crate) trait RuntimeBoundaryHook: Send + Sync {
    fn acquire(
        &self,
        context: PublicSimulationContext,
        request: AcquireAuthorityRequest,
    ) -> BoundaryFuture<()>;
    fn advance(
        &self,
        context: PublicSimulationContext,
        request: AdvanceRequest,
    ) -> BoundaryFuture<AdvanceResponse>;
    fn reset(
        &self,
        context: PublicSimulationContext,
        request: ResetRequest,
        next_timeline_id: String,
    ) -> BoundaryFuture<()>;
    fn release(
        &self,
        context: PublicSimulationContext,
        request: ReleaseAuthorityRequest,
    ) -> BoundaryFuture<()>;
    fn progress(
        &self,
        context: PublicSimulationContext,
        request: ProgressRequest,
    ) -> BoundaryFuture<ProgressResponse>;
}

type BoundaryFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send>>;

/// Explicit boundary absence used by hardware-only supervisors.
#[derive(Debug, Default)]
pub(crate) struct NoControlledRuntimeBoundary;

impl RuntimeBoundaryHook for NoControlledRuntimeBoundary {
    fn acquire(
        &self,
        _context: PublicSimulationContext,
        _request: AcquireAuthorityRequest,
    ) -> BoundaryFuture<()> {
        Box::pin(async { Ok(()) })
    }

    fn advance(
        &self,
        _context: PublicSimulationContext,
        _request: AdvanceRequest,
    ) -> BoundaryFuture<AdvanceResponse> {
        Box::pin(async {
            Err("the selected Runtime has no controlled boundary hook".to_owned())
        })
    }

    fn reset(
        &self,
        _context: PublicSimulationContext,
        _request: ResetRequest,
        _next_timeline_id: String,
    ) -> BoundaryFuture<()> {
        Box::pin(async {
            Err("the selected Runtime has no controlled boundary hook".to_owned())
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
        _context: PublicSimulationContext,
        _request: ProgressRequest,
    ) -> BoundaryFuture<ProgressResponse> {
        Box::pin(async {
            Err("the selected Runtime has no controlled boundary hook".to_owned())
        })
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AdvanceIdentity {
    session_id: Vec<u8>,
    authority_grant: Vec<u8>,
    execution_id: String,
    timeline_id: String,
    correlation_id: Vec<u8>,
}

struct PendingAdvance {
    digest: [u8; 32],
    result: watch::Receiver<Option<Result<AdvanceResponse, PublicBackendError>>>,
}

/// Concrete simulation bridge which validates the immutable bundle contract,
/// forwards observations to Runtime publication ports, and delegates exactly
/// one admitted boundary to [`RuntimeBoundaryHook`].
pub(crate) struct RuntimeSimulationBridge {
    bus: BusHandle,
    ports: Arc<BTreeMap<(String, String), RuntimePortContract>>,
    definition: Option<SimulationDefinition>,
    boundary: Arc<dyn RuntimeBoundaryHook>,
    next_sequence: Arc<AtomicU64>,
    advances: Arc<Mutex<BTreeMap<AdvanceIdentity, PendingAdvance>>>,
}

impl std::fmt::Debug for RuntimeSimulationBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeSimulationBridge")
            .field("ports", &self.ports.len())
            .field("has_definition", &self.definition.is_some())
            .finish_non_exhaustive()
    }
}

impl RuntimeSimulationBridge {
    pub(crate) fn new(
        bus: BusHandle,
        surface: &RuntimePublicSurface,
        definition: Option<SimulationDefinition>,
        boundary: Arc<dyn RuntimeBoundaryHook>,
    ) -> Self {
        Self {
            bus,
            ports: surface.ports.clone(),
            definition,
            boundary,
            next_sequence: Arc::new(AtomicU64::new(1)),
            advances: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn ensure_definition(&self, context: &PublicSimulationContext) -> Result<&SimulationDefinition, PublicBackendError> {
        let definition = self.definition.as_ref().ok_or_else(|| {
            PublicBackendError::RejectedBeforeAdmission(
                "the admitted bundle has no immutable simulation definition".to_owned(),
            )
        })?;
        if definition.model_identity() != context.model_identity
            || definition.quantum_ns() != context.quantum_ns
        {
            return Err(PublicBackendError::RejectedBeforeAdmission(
                "simulation context does not match the immutable bundle definition".to_owned(),
            ));
        }
        Ok(definition)
    }

    fn validate_provider_set(
        &self,
        definition: &SimulationDefinition,
        request: &AdvanceRequest,
    ) -> Result<(), PublicBackendError> {
        let required = definition
            .providers()
            .iter()
            .map(|provider| (provider.service_instance(), provider.port()))
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        for observation in &request.observations {
            if !seen.insert((
                observation.service_instance.as_str(),
                observation.port.as_str(),
            )) {
                return Err(PublicBackendError::RejectedBeforeAdmission(
                    "simulation observations contain a duplicate provider".to_owned(),
                ));
            }
            let provider = definition
                .providers()
                .iter()
                .find(|provider| {
                    provider.service_instance() == observation.service_instance
                        && provider.port() == observation.port
                })
                .ok_or_else(|| {
                    PublicBackendError::RejectedBeforeAdmission(
                        "simulation observation is not in the immutable provider set".to_owned(),
                    )
                })?;
            let contract = self
                .ports
                .get(&(observation.service_instance.clone(), observation.port.clone()))
                .ok_or_else(|| {
                    PublicBackendError::RejectedBeforeAdmission(
                        "simulation provider is absent from the compiled Runtime graph".to_owned(),
                    )
                })?;
            if contract.metadata.kind != provider.kind() as i32
                || contract.metadata.input_fqn != provider.input_fqn()
                || contract.metadata.output_fqn != provider.payload_fqn()
                || observation.payload.len() as u64 > contract.response_max_bytes
            {
                return Err(PublicBackendError::RejectedBeforeAdmission(
                    "simulation observation does not match its immutable provider metadata"
                        .to_owned(),
                ));
            }
        }
        if seen != required {
            return Err(PublicBackendError::RejectedBeforeAdmission(
                "simulation observations do not contain the complete immutable provider set"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    async fn publish_observations(&self, request: &AdvanceRequest) -> Result<(), PublicBackendError> {
        let session = self
            .bus
            .session()
            .map_err(|error| PublicBackendError::Transport(error.to_string()))?;
        for observation in &request.observations {
            let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
            if sequence == 0 {
                return Err(PublicBackendError::RejectedBeforeAdmission(
                    "simulation observation sequence exhausted".to_owned(),
                ));
            }
            let stamp = ObservationStamp::new(
                "simulation",
                ExecutionTime::from_nanos(observation.capture_time_ns),
                None,
            );
            let metadata = RuntimeWireMetadata::observed(&stamp, sequence);
            let attachment = encode_runtime_metadata(&metadata)?;
            let key = self.bus.full_key(&port_key(
                &observation.service_instance,
                &observation.port,
                "publish",
            ));
            session
                .put(key, observation.payload.clone())
                .encoding(Encoding::from(PROTOBUF_ENCODING.to_owned()))
                .attachment(attachment)
                .await
                .map_err(|error| PublicBackendError::Transport(error.to_string()))?;
        }
        Ok(())
    }

    async fn advance_inner(
        self: Arc<Self>,
        context: PublicSimulationContext,
        request: AdvanceRequest,
    ) -> Result<AdvanceResponse, PublicBackendError> {
        let definition = self.ensure_definition(&context)?;
        self.validate_provider_set(definition, &request)?;
        let digest: [u8; 32] = Sha256::digest(request.encode_to_vec()).into();
        let identity = AdvanceIdentity {
            session_id: context.session_id.clone(),
            authority_grant: context.authority_grant.clone(),
            execution_id: context.execution_id.clone(),
            timeline_id: context.timeline_id.clone(),
            correlation_id: context.correlation_id.clone(),
        };
        let (sender, mut receiver, execute) = {
            let mut advances = lock_unpoisoned(&self.advances);
            if let Some(pending) = advances.get(&identity) {
                if pending.digest != digest {
                    return Err(PublicBackendError::RejectedBeforeAdmission(
                        "simulation correlation_id was reused with different inputs".to_owned(),
                    ));
                }
                (None, pending.result.clone(), false)
            } else {
                if advances.len() >= MAX_RETAINED_ADVANCES {
                    let Some(oldest) = advances
                        .iter()
                        .find(|(_, pending)| pending.result.borrow().is_some())
                        .map(|(identity, _)| identity.clone())
                    else {
                        return Err(PublicBackendError::Capacity);
                    };
                    advances.remove(&oldest);
                }
                let (sender, receiver) = watch::channel(None);
                advances.insert(
                    identity,
                    PendingAdvance {
                        digest,
                        result: receiver.clone(),
                    },
                );
                (Some(sender), receiver, true)
            }
        };
        if execute {
            let Some(sender) = sender else {
                return Err(PublicBackendError::Transport(
                    "simulation advance admission lost its result channel".to_owned(),
                ));
            };
            let bridge = Arc::clone(&self);
            tokio::spawn(async move {
                let operation = async {
                    bridge.publish_observations(&request).await?;
                    bridge
                        .boundary
                        .advance(context, request)
                        .await
                        .map_err(PublicBackendError::Transport)
                }
                .await;
                let _ = sender.send(Some(operation));
            });
        }
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            if receiver.changed().await.is_err() {
                return Err(PublicBackendError::Transport(
                    "simulation boundary result was abandoned".to_owned(),
                ));
            }
        }
    }
}

impl PublicSimulationBackend for RuntimeSimulationBridge {
    fn acquire(
        &self,
        context: PublicSimulationContext,
        request: AcquireAuthorityRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicBackendError>> + Send>> {
        let definition = self.definition.clone();
        let boundary = self.boundary.clone();
        Box::pin(async move {
            let definition = definition.ok_or_else(|| {
                PublicBackendError::RejectedBeforeAdmission(
                    "the admitted bundle has no immutable simulation definition".to_owned(),
                )
            })?;
            if request.model_identity != definition.model_identity()
                || request.quantum_ns != definition.quantum_ns()
                || request.providers.len() != definition.providers().len()
            {
                return Err(PublicBackendError::RejectedBeforeAdmission(
                    "simulation authority does not match the immutable bundle definition"
                        .to_owned(),
                ));
            }
            boundary
                .acquire(context, request)
                .await
                .map_err(PublicBackendError::Transport)
        })
    }

    fn advance(
        &self,
        context: PublicSimulationContext,
        request: AdvanceRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AdvanceResponse, PublicBackendError>> + Send>> {
        let bridge = Arc::new(self.clone_for_async());
        Box::pin(async move { bridge.advance_inner(context, request).await })
    }

    fn reset(
        &self,
        context: PublicSimulationContext,
        request: ResetRequest,
        next_timeline_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicBackendError>> + Send>> {
        let definition = self.definition.clone();
        let boundary = self.boundary.clone();
        Box::pin(async move {
            if definition.is_none() {
                return Err(PublicBackendError::RejectedBeforeAdmission(
                    "the admitted bundle has no immutable simulation definition".to_owned(),
                ));
            }
            boundary
                .reset(context, request, next_timeline_id)
                .await
                .map_err(PublicBackendError::Transport)
        })
    }

    fn release(
        &self,
        context: PublicSimulationContext,
        request: ReleaseAuthorityRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicBackendError>> + Send>> {
        let boundary = self.boundary.clone();
        Box::pin(async move {
            boundary
                .release(context, request)
                .await
                .map_err(PublicBackendError::Transport)
        })
    }

    fn progress(
        &self,
        context: PublicSimulationContext,
        request: ProgressRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ProgressResponse, PublicBackendError>> + Send>> {
        let definition = self.definition.clone();
        let boundary = self.boundary.clone();
        Box::pin(async move {
            if definition.is_none() {
                return Err(PublicBackendError::RejectedBeforeAdmission(
                    "the admitted bundle has no immutable simulation definition".to_owned(),
                ));
            }
            boundary
                .progress(context, request)
                .await
                .map_err(PublicBackendError::Transport)
        })
    }
}

impl RuntimeSimulationBridge {
    fn clone_for_async(&self) -> Self {
        Self {
            bus: self.bus.clone(),
            ports: self.ports.clone(),
            definition: self.definition.clone(),
            boundary: self.boundary.clone(),
            next_sequence: self.next_sequence.clone(),
            advances: self.advances.clone(),
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Debug, Deserialize)]
struct ArtifactSummary {
    runtime: ArtifactRuntime,
}

#[derive(Debug, Deserialize)]
struct ArtifactRuntime {
    #[serde(default)]
    inputs: Vec<ArtifactInput>,
    #[serde(default)]
    transient_outputs: Vec<ArtifactOutput>,
    #[serde(default)]
    service_outputs: Vec<ArtifactOutput>,
}

#[derive(Debug, Deserialize)]
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
    #[serde(default)]
    signature: Option<ArtifactSignature>,
}

#[derive(Debug, Deserialize)]
struct ArtifactOutput {
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    port: Option<String>,
    #[serde(default)]
    signature: Option<ArtifactSignature>,
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    max_items: Option<u64>,
    #[serde(default)]
    max_bytes: Option<u64>,
    #[serde(default)]
    max_request_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ArtifactSignature {
    name: String,
    service: String,
    method: String,
    kind: String,
    request: String,
    response: String,
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    use tokio::sync::Notify;

    use super::*;

    #[test]
    fn metadata_kind_mapping_is_explicit() {
        assert_eq!(output_kind("state"), Some(PortKind::State));
        assert_eq!(output_kind("read"), Some(PortKind::Read));
        assert_eq!(output_kind("commands"), None);
    }

    #[test]
    fn runtime_metadata_is_bounded_before_encoding() {
        let metadata = RuntimeWireMetadata {
            source: Some("x".repeat(2_000)),
            ..RuntimeWireMetadata::default()
        };
        assert!(encode_runtime_metadata(&metadata).is_err());
    }

    #[test]
    fn public_surface_uses_brain_artifact_and_exact_service_bounds() {
        let brain_artifact = serde_json::json!({
            "runtime": {
                "service_outputs": [{
                    "name": "state_output",
                    "kind": "state",
                    "port": "state",
                    "max_items": 1,
                    "max_bytes": 64,
                    "signature": {
                        "name": "state",
                        "service": "fixture.Brain",
                        "method": "State",
                        "kind": "state",
                        "request": "google.protobuf.Empty",
                        "response": "fixture.State"
                    }
                }]
            }
        });
        let service_artifact = serde_json::json!({
            "runtime": {
                "inputs": [{
                    "name": "request",
                    "kind": "commands",
                    "port": "command",
                    "max_items": 2,
                    "max_bytes": 128,
                    "signature": {
                        "name": "command",
                        "service": "fixture.Service",
                        "method": "Command",
                        "kind": "commands",
                        "request": "fixture.Request",
                        "response": "fixture.Response"
                    }
                }],
                "service_outputs": [{
                    "name": "reply",
                    "kind": "reply",
                    "input": "request",
                    "max_bytes": 256
                }]
            }
        });
        let manifest = super::super::bundle::SourceManifest::for_test(
            "robot",
            vec![
                super::super::bundle::SourceExecutable::for_test_with_artifact(
                    "brain",
                    brain_artifact,
                ),
                super::super::bundle::SourceExecutable::for_test_with_artifact(
                    "service",
                    service_artifact,
                ),
            ],
        );
        let bundle = Bundle::Source(SourceBundle::for_test(
            std::path::Path::new("/tmp/fixture"),
            manifest,
        ));
        let surface = RuntimePublicSurface::from_bundle(&bundle).expect("artifact contracts");

        assert_eq!(
            surface
                .services
                .iter()
                .map(ServicePorts::instance)
                .collect::<Vec<_>>(),
            vec!["brain", "service"]
        );
        let brain_state = surface
            .ports
            .get(&("brain".to_owned(), "state".to_owned()))
            .expect("brain public port");
        assert_eq!(brain_state.metadata.input_fqn, "google.protobuf.Empty");
        assert_eq!(brain_state.metadata.output_fqn, "fixture.State");
        assert_eq!(brain_state.metadata.max_message_bytes, 64);

        let command = surface
            .ports
            .get(&("service".to_owned(), "command".to_owned()))
            .expect("service command");
        assert_eq!(command.metadata.max_message_bytes, 256);
        assert_eq!(command.request_max_bytes, 128);
        assert_eq!(command.response_max_bytes, 256);
    }

    #[derive(Debug)]
    struct TestExternalIngress {
        tickets: Mutex<VecDeque<ExternalIngressTicket>>,
        seen: Mutex<Vec<(String, String, String)>>,
    }

    impl RuntimeExternalIngress for TestExternalIngress {
        fn admit(
            &self,
            target_instance: &str,
            target_port: &str,
            caller: &RuntimeIngressIdentity,
            _contract: &RuntimePortContract,
        ) -> Result<ExternalIngressTicket, PublicBackendError> {
            assert_eq!(caller.source, "supervisor");
            assert_eq!(caller.caller, "supervisor.public");
            self.seen.lock().expect("test lock").push((
                target_instance.to_owned(),
                target_port.to_owned(),
                caller.caller.clone(),
            ));
            self.tickets
                .lock()
                .expect("test lock")
                .pop_front()
                .ok_or(PublicBackendError::Capacity)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn external_ingress_carries_exact_boundary_and_sequence_order() {
        let (owner, bus) = crate::bus::BusOwner::open(crate::bus::BusConfig::for_external(
            crate::identity::ExecutionId::mint(),
            None,
            Vec::new(),
        ))
        .await
        .expect("test bus opens");
        let metadata = PortMetadata {
            name: "command".to_owned(),
            kind: PortKind::Commands as i32,
            input_fqn: "fixture.Request".to_owned(),
            output_fqn: "fixture.Response".to_owned(),
            max_message_bytes: 256,
            max_buffered_items: 2,
        };
        let contract = RuntimePortContract {
            metadata: metadata.clone(),
            request_max_bytes: 128,
            response_max_bytes: 256,
        };
        let surface = RuntimePublicSurface {
            services: Vec::new(),
            ports: Arc::new(BTreeMap::from([(
                ("service".to_owned(), "command".to_owned()),
                contract,
            )])),
            ingress: RuntimeIngressIdentity::default(),
        };
        let external = Arc::new(TestExternalIngress {
            tickets: Mutex::new(VecDeque::from([
                ExternalIngressTicket {
                    eligible_boundary: 17,
                    ingress_sequence: 42,
                },
                ExternalIngressTicket {
                    eligible_boundary: 17,
                    ingress_sequence: 43,
                },
            ])),
            seen: Mutex::new(Vec::new()),
        });
        let backend = RuntimePublicBackend::new(bus.clone(), &surface, external.clone());
        let session = bus.session().expect("bus session");
        let request_key = bus.full_key(&port_key("service", "command", "request"));
        let reply_key = bus.full_key(&port_key("service", "command", "reply"));
        let request_subscriber = session
            .declare_subscriber(OwnedKeyExpr::new(request_key).expect("request key"))
            .with(zenoh::handlers::FifoChannel::new(4))
            .await
            .expect("request subscriber");
        let responder = tokio::spawn(async move {
            for expected_sequence in [42_u64, 43_u64] {
                let sample = request_subscriber
                    .recv_async()
                    .await
                    .expect("request sample");
                let wire = WireSample::from_zenoh(sample).expect("request metadata");
                assert_eq!(wire.metadata().eligible_boundary, Some(17));
                assert_eq!(wire.metadata().sequence, Some(expected_sequence));
                assert_eq!(wire.metadata().caller.as_deref(), Some("supervisor.public"));
                let mut response_metadata = wire.metadata().clone();
                response_metadata.source = Some("service".to_owned());
                let attachment = encode_runtime_metadata(&response_metadata).expect("reply metadata");
                session
                    .put(reply_key.clone(), vec![expected_sequence as u8])
                    .encoding(Encoding::from(PROTOBUF_ENCODING.to_owned()))
                    .attachment(attachment)
                    .await
                    .expect("reply sample");
            }
        });
        let binding = PublicBindingContext {
            session_id: vec![1],
            binding_id: vec![2],
            execution_id: "execution".to_owned(),
            timeline_id: "timeline".to_owned(),
            service_instance: "service".to_owned(),
            metadata,
        };
        let first = backend
            .call_inner(
                PublicOperation::Command,
                binding.clone(),
                vec![1],
                Duration::from_secs(1),
            )
            .await
            .expect("first call");
        let second = backend
            .call_inner(
                PublicOperation::Command,
                binding,
                vec![2],
                Duration::from_secs(1),
            )
            .await
            .expect("second call");
        assert_eq!(first, PublicBackendOutcome::Received(vec![42]));
        assert_eq!(second, PublicBackendOutcome::Received(vec![43]));
        responder.await.expect("responder completes");
        assert_eq!(
            external.seen.lock().expect("test lock").as_slice(),
            [
                (
                    "service".to_owned(),
                    "command".to_owned(),
                    "supervisor.public".to_owned(),
                ),
                (
                    "service".to_owned(),
                    "command".to_owned(),
                    "supervisor.public".to_owned(),
                ),
            ]
        );
        owner.close().await;
    }

    #[derive(Debug)]
    struct DelayedBoundary {
        calls: AtomicUsize,
        release: Arc<Notify>,
    }

    impl RuntimeBoundaryHook for DelayedBoundary {
        fn acquire(
            &self,
            _context: PublicSimulationContext,
            _request: AcquireAuthorityRequest,
        ) -> BoundaryFuture<()> {
            Box::pin(async { Ok(()) })
        }

        fn advance(
            &self,
            context: PublicSimulationContext,
            request: AdvanceRequest,
        ) -> BoundaryFuture<AdvanceResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let release = Arc::clone(&self.release);
            Box::pin(async move {
                release.notified().await;
                Ok(AdvanceResponse {
                    completed_boundary: context.completed_boundary.saturating_add(1),
                    observation_receipts: request
                        .observations
                        .iter()
                        .map(|observation| {
                            crate::communication::simulation::ProductReceipt {
                                service_instance: observation.service_instance.clone(),
                                port: observation.port.clone(),
                                ..Default::default()
                            }
                        })
                        .collect(),
                    ..Default::default()
                })
            })
        }

        fn reset(
            &self,
            _context: PublicSimulationContext,
            _request: ResetRequest,
            _next_timeline_id: String,
        ) -> BoundaryFuture<()> {
            Box::pin(async { Ok(()) })
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
            _context: PublicSimulationContext,
            _request: ProgressRequest,
        ) -> BoundaryFuture<ProgressResponse> {
            Box::pin(async { Err("not used in this test".to_owned()) })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn uncertain_advance_retries_the_same_boundary_once() {
        let (owner, bus) = crate::bus::BusOwner::open(crate::bus::BusConfig::for_external(
            crate::identity::ExecutionId::mint(),
            None,
            Vec::new(),
        ))
        .await
        .expect("test bus opens");
        let metadata = PortMetadata {
            name: "state".to_owned(),
            kind: PortKind::State as i32,
            input_fqn: "google.protobuf.Empty".to_owned(),
            output_fqn: "example.Payload".to_owned(),
            max_message_bytes: 1024,
            max_buffered_items: 1,
        };
        let contract = RuntimePortContract {
            metadata: metadata.clone(),
            request_max_bytes: 1024,
            response_max_bytes: 1024,
        };
        let surface = RuntimePublicSurface {
            services: Vec::new(),
            ports: Arc::new(BTreeMap::from([(
                ("sensor".to_owned(), "state".to_owned()),
                contract,
            )])),
            ingress: RuntimeIngressIdentity::default(),
        };
        let definition = SimulationDefinition::new(
            "model-digest",
            1_000,
            vec![crate::communication::SimulationProviderDefinition::new(
                "sensor",
                "state",
                PortKind::State,
                "google.protobuf.Empty",
                "example.Payload",
            )
            .expect("provider")],
        )
        .expect("simulation definition");
        let boundary = Arc::new(DelayedBoundary {
            calls: AtomicUsize::new(0),
            release: Arc::new(Notify::new()),
        });
        let bridge = RuntimeSimulationBridge::new(
            bus,
            &surface,
            Some(definition),
            boundary.clone(),
        );
        let context = PublicSimulationContext {
            principal: "simulator".to_owned(),
            session_id: vec![1],
            authority_grant: vec![2],
            correlation_id: vec![3],
            execution_id: "execution".to_owned(),
            timeline_id: "timeline".to_owned(),
            completed_boundary: 4,
            model_identity: "model-digest".to_owned(),
            quantum_ns: 1_000,
        };
        let request = AdvanceRequest {
            authority_grant: vec![2],
            execution_id: "execution".to_owned(),
            timeline_id: "timeline".to_owned(),
            boundary: 4,
            observations: vec![crate::communication::simulation::Observation {
                service_instance: "sensor".to_owned(),
                port: "state".to_owned(),
                capture_time_ns: 4_000,
                payload: vec![9],
            }],
            session_id: vec![1],
            correlation_id: vec![3],
        };
        let first = tokio::time::timeout(
            Duration::from_millis(20),
            bridge.advance(context.clone(), request.clone()),
        )
        .await;
        assert!(first.is_err(), "the first caller must observe an uncertain timeout");
        for _ in 0..100 {
            if boundary.calls.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(boundary.calls.load(Ordering::SeqCst), 1);
        let retry = bridge.advance(context, request);
        boundary.release.notify_waiters();
        let response = tokio::time::timeout(Duration::from_secs(1), retry)
            .await
            .expect("retry completes")
            .expect("boundary succeeds");
        assert_eq!(response.completed_boundary, 5);
        assert_eq!(boundary.calls.load(Ordering::SeqCst), 1);
        owner.close().await;
    }
}
