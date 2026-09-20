//! Receiver queues, immutable input cuts, and command admission.
use super::exchange::{
    ActivationStateMap, CorrelationMap, ExchangeCompletion, ExchangeCompletionQueue,
    ExpiredCorrelationSet, OperationQueue,
};
use super::{
    InputDirection, InputSource, ResolvedInputRoute, RuntimeInputReceipt, RuntimeLaunchManifest,
    parse_graph_endpoint,
};
use crate::runtime::core::RegisteredRuntime;
use crate::runtime::execution_protocol::{self, wire as execution_wire};
use crate::runtime::input::{InputSnapshot, TransportInputSet, TransportKeyLookup, TransportValue};
use crate::runtime::schedule::HardwareInvocation;
use crate::runtime::transport::{self, TransportError, WireSample};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use zenoh::bytes::Encoding;

pub(super) struct ExecutionInputAdapter<R> {
    pub(super) bus: Option<crate::runtime::connection::Connection>,
    pub(super) subscriptions: Vec<BoundSubscription>,
    pub(super) command_high_watermarks: BTreeMap<(String, String, String), u64>,
    pub(super) external_ingress_high_watermarks: BTreeMap<String, u64>,
    pub(super) future_commands: BTreeMap<&'static str, Vec<WireSample>>,
    pub(super) command_ranks: BTreeMap<(String, String), u64>,
    pub(super) correlations: Option<CorrelationMap>,
    pub(super) expired_correlations: Option<ExpiredCorrelationSet>,
    pub(super) operation_completions: Option<OperationQueue>,
    pub(super) exchange_completions: Option<ExchangeCompletionQueue>,
    pub(super) activation_states: Option<ActivationStateMap>,
    pub(super) managed_inputs: BTreeMap<&'static str, TransportValue>,
    pub(super) observed_attempts: BTreeMap<&'static str, u64>,
    pub(super) stream_terminal: BTreeSet<&'static str>,
    pub(super) last_input_receipts: Vec<RuntimeInputReceipt>,
    pub(super) stopped: bool,
    pub(super) _runtime: PhantomData<fn() -> R>,
}

pub(super) type RuntimeSubscription =
    zenoh::pubsub::Subscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>;

pub(super) struct BoundSubscription {
    pub(super) field: &'static str,
    pub(super) binding: crate::runtime::transport::PortBinding,
    pub(super) direction: InputDirection,
    pub(super) max_items: u64,
    pub(super) max_bytes: u64,
    /// The direct subscriber remains available to the in-process test path.
    /// Process-bound subscriptions are drained by `delivery_receive_loop`
    /// into the receiver-owned bounded queue below.
    pub(super) subscriber: Option<RuntimeSubscription>,
    pub(super) delivery: Option<DeliverySubscription>,
}

/// One receiver-owned queue and its cancellation fence.
///
/// The queue is separate from Zenoh's subscriber handler.  A record is
/// acknowledged only after this queue has admitted it, so producer-side
/// publication completion cannot be mistaken for receiver admission.
pub(super) struct DeliverySubscription {
    pub(super) queue: Arc<Mutex<DeliveryQueue>>,
    pub(super) cancel: CancellationToken,
    pub(super) expected: Arc<AtomicBool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct DeliveryIdentity {
    pub(super) execution_id: String,
    pub(super) timeline_id: String,
    pub(super) boundary: u64,
    pub(super) source: String,
    pub(super) target: String,
    pub(super) port: String,
    pub(super) direction: String,
    pub(super) sequence: u64,
    pub(super) item: u32,
    pub(super) bytes: u64,
    pub(super) payload_digest: [u8; 32],
}

pub(super) struct DeliveryQueue {
    pub(super) items: VecDeque<WireSample>,
    pub(super) bytes: u64,
    pub(super) max_items: u64,
    pub(super) max_bytes: u64,
    pub(super) input_kind: crate::runtime::input::InputKind,
    pub(super) timeline_id: Option<String>,
    /// One high-water mark per immutable source route.  Controlled records
    /// are ordered by boundary first, then producer sequence and item.  This
    /// gives duplicate delivery idempotence without retaining every identity
    /// for the lifetime of a long-running runtime.
    pub(super) high_watermarks: BTreeMap<(String, String, String, String), DeliveryIdentity>,
}

impl DeliveryQueue {
    pub(super) fn new(
        max_items: u64,
        max_bytes: u64,
        input_kind: crate::runtime::input::InputKind,
    ) -> Self {
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

    pub(super) fn set_timeline(&mut self, timeline_id: &str) {
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

    pub(super) fn retain_timeline(&mut self, timeline_id: &str) {
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

    pub(super) fn clear(&mut self) {
        self.items.clear();
        self.bytes = 0;
        self.high_watermarks.clear();
    }

    pub(super) fn accepts_timeline(&self, timeline_id: &str) -> bool {
        self.timeline_id
            .as_deref()
            .is_some_and(|current| current == timeline_id)
    }

    pub(super) fn identity(
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
            source: metadata.publisher().map(str::to_owned).ok_or_else(|| {
                TransportError::InvalidMetadata {
                    detail: "required delivery is missing source identity".to_owned(),
                }
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
            payload_digest: Sha256::digest(sample.payload()).into(),
        })
    }

    pub(super) fn admit(
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
                if order == std::cmp::Ordering::Equal
                    && (previous.bytes != identity.bytes
                        || previous.payload_digest != identity.payload_digest)
                {
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
            crate::runtime::input::InputKind::Latest | crate::runtime::input::InputKind::Setpoint
        );
        if !is_replaceable && self.items.len() as u64 >= self.max_items {
            return Err(DeliveryAdmissionError::Saturated(format!(
                "receiver queue item capacity {} is exhausted",
                self.max_items
            )));
        }
        if is_replaceable {
            if bytes > self.max_bytes {
                return Err(DeliveryAdmissionError::Saturated(format!(
                    "receiver value exceeds byte capacity {}",
                    self.max_bytes
                )));
            }
            // Keep the newest value for each of the two adjacent eligibility
            // cuts. A fast producer must not erase the value a slower process
            // is about to freeze for the current boundary.
            let eligible = sample.metadata().eligible_boundary;
            self.items
                .retain(|old| old.metadata().eligible_boundary != eligible);
            while self.items.len() >= 2 {
                self.items.pop_front();
            }
            self.items.push_back(sample);
            self.bytes = self
                .items
                .iter()
                .map(|item| item.payload().len() as u64)
                .sum();
        } else {
            let next_bytes = self.bytes.checked_add(bytes).ok_or_else(|| {
                DeliveryAdmissionError::Saturated("receiver queue byte count overflowed".to_owned())
            })?;
            if next_bytes > self.max_bytes {
                return Err(DeliveryAdmissionError::Saturated(format!(
                    "receiver queue byte capacity {} is exhausted",
                    self.max_bytes
                )));
            }
            self.bytes = next_bytes;
            self.items.push_back(sample);
        }
        self.high_watermarks.insert(route, identity.clone());
        Ok((identity, true))
    }

    /// Admit a normal hardware or public-ingress record.  These records do
    /// not carry controlled execution identity and therefore have no private
    /// acknowledgement leg, but they still enter the same receiver-owned
    /// bounded queue and obey replacement/accumulation semantics.
    pub(super) fn admit_untracked(
        &mut self,
        sample: WireSample,
    ) -> Result<(), DeliveryAdmissionError> {
        let bytes = sample.payload().len() as u64;
        let is_replaceable = matches!(
            self.input_kind,
            crate::runtime::input::InputKind::Latest | crate::runtime::input::InputKind::Setpoint
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

    pub(super) fn drain(&mut self, boundary: u64) -> Vec<WireSample> {
        let mut selected = Vec::new();
        self.items.retain(|sample| {
            let metadata = sample.metadata();
            // Untracked hardware/public records keep their existing handling.
            if metadata.execution_id.is_none()
                || metadata
                    .eligible_boundary
                    .is_none_or(|eligible| eligible <= boundary)
            {
                selected.push(sample.clone());
                false
            } else {
                true
            }
        });
        if matches!(
            self.input_kind,
            crate::runtime::input::InputKind::Latest | crate::runtime::input::InputKind::Setpoint
        ) {
            if selected.len() > 1 {
                selected.drain(..selected.len() - 1);
            }
            // Latest and Setpoint remain available until replaced or expired.
            // Keep the wire stamp so freshness is checked at every freeze.
            if let Some(current) = selected.last() {
                self.items.push_front(current.clone());
            }
        }
        self.bytes = self
            .items
            .iter()
            .map(|item| item.payload().len() as u64)
            .sum();
        selected
    }
}

/// Compute one receiver-owned reservation for a complete input field.  A
/// fan-in field has one aggregate budget, not one budget per producer route.
/// Latest and Setpoint inputs replace their retained value, while all other
/// input forms accumulate records until the next invocation freezes them.
pub(super) fn aggregate_delivery_capacity(
    field: &crate::runtime::transport::InputTransportField,
    routes: &[ResolvedInputRoute],
) -> (u64, u64) {
    let replaceable = matches!(
        field.kind,
        crate::runtime::input::InputKind::Latest | crate::runtime::input::InputKind::Setpoint
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
pub(super) enum DeliveryAdmissionError {
    Malformed(TransportError),
    Saturated(String),
}

pub(super) struct DeliveryReceiver {
    pub(super) subscriber: RuntimeSubscription,
    pub(super) queue: Arc<Mutex<DeliveryQueue>>,
    pub(super) bus: crate::runtime::connection::Connection,
    pub(super) expected_source: String,
    pub(super) expected_callers: BTreeSet<String>,
    pub(super) target: String,
    pub(super) port: String,
    pub(super) direction: String,
    pub(super) ack_leg: String,
    pub(super) cancel: CancellationToken,
    pub(super) reply_admission: Option<ReplyAdmission>,
}

pub(super) struct ReplyAdmission {
    pub(super) field: &'static str,
    pub(super) caller_rank: u64,
    pub(super) caller: String,
    pub(super) correlations: CorrelationMap,
}

impl ReplyAdmission {
    pub(super) fn admit(&self, command_id: Option<u64>, caller_rank: Option<u64>) {
        if caller_rank != Some(self.caller_rank) {
            return;
        }
        let Some(command_id) = command_id else {
            return;
        };
        let mut correlations = self
            .correlations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(pending) = correlations.get_mut(&(self.field.to_owned(), command_id))
            && pending
                .deadline
                .is_none_or(|deadline| Instant::now() <= deadline)
        {
            pending.reply_admitted = true;
        }
    }
}

impl DeliveryIdentity {
    pub(super) fn route_key(&self) -> (String, String, String, String) {
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
pub(super) async fn delivery_receive_loop(receiver: DeliveryReceiver) -> crate::Result<()> {
    let DeliveryReceiver {
        subscriber,
        queue,
        bus,
        expected_source,
        expected_callers,
        target,
        port,
        direction,
        ack_leg,
        cancel,
        reply_admission,
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
        let reply_identity = (metadata.command_id, metadata.caller_rank);
        if let Some(reply) = &reply_admission {
            if metadata.caller.as_deref() != Some(reply.caller.as_str()) {
                continue;
            }
            if metadata.caller_rank != Some(reply.caller_rank) {
                return Err(anyhow::anyhow!(TransportError::CommandCorrelation(
                    "reply recipient and caller ordinal disagree".to_owned()
                )));
            }
        }

        let source = metadata
            .publisher()
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
            if queue.admit_untracked(wire).is_ok()
                && let Some(reply) = &reply_admission
            {
                reply.admit(reply_identity.0, reply_identity.1);
            }
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
                &ack_leg,
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
                &ack_leg,
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
                if let Some(reply) = &reply_admission {
                    reply.admit(reply_identity.0, reply_identity.1);
                }
                publish_delivery_ack(&bus, &identity, &ack_leg, true, None).await?;
            }
            Err(DeliveryAdmissionError::Saturated(detail)) => {
                publish_delivery_ack(&bus, &identity, &ack_leg, false, Some(detail)).await?;
            }
            Err(DeliveryAdmissionError::Malformed(error)) => {
                return Err(anyhow::anyhow!(error));
            }
        }
    }
}

pub(super) fn validate_controlled_request_source(
    metadata: &crate::runtime::transport::RuntimeWireMetadata,
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

pub(super) async fn publish_delivery_ack(
    bus: &crate::runtime::connection::Connection,
    identity: &DeliveryIdentity,
    leg: &str,
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
    let key = execution_protocol::key(bus, &identity.source, leg);
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

pub(super) struct CollectedInput {
    pub(super) field: &'static str,
    pub(super) binding: crate::runtime::transport::PortBinding,
    pub(super) direction: InputDirection,
    pub(super) max_items: u64,
    pub(super) max_bytes: u64,
    pub(super) samples: Vec<WireSample>,
}

impl<R> ExecutionInputAdapter<R> {
    pub(super) fn unbound() -> Self {
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
            activation_states: Some(Arc::new(Mutex::new(BTreeMap::new()))),
            managed_inputs: BTreeMap::new(),
            observed_attempts: BTreeMap::new(),
            stream_terminal: BTreeSet::new(),
            last_input_receipts: Vec::new(),
            stopped: false,
            _runtime: PhantomData,
        }
    }

    pub(super) fn with_shared_state(
        mut self,
        correlations: CorrelationMap,
        expired_correlations: ExpiredCorrelationSet,
        operation_completions: OperationQueue,
        exchange_completions: ExchangeCompletionQueue,
        activation_states: ActivationStateMap,
    ) -> Self {
        self.correlations = Some(correlations);
        self.expired_correlations = Some(expired_correlations);
        self.operation_completions = Some(operation_completions);
        self.exchange_completions = Some(exchange_completions);
        self.activation_states = Some(activation_states);
        self
    }

    pub(super) async fn bind(
        &mut self,
        bus: crate::runtime::connection::Connection,
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
                let ack_leg = if route.direction == InputDirection::Reply
                    && route.binding.kind == crate::port::PortKind::Read
                {
                    format!("read-reply-ack/{}", route.source_port)
                } else {
                    "delivery-ack".to_owned()
                };
                let worker_target = format!("{}.{}", manifest.instance_id, route.field);
                let worker_port = route.binding.name.clone();
                let worker_direction = route.direction.key_direction().to_owned();
                let reply_admission = if route.direction == InputDirection::Reply {
                    self.correlations.as_ref().zip(route.caller_rank).map(
                        |(correlations, caller_rank)| ReplyAdmission {
                            field: route.field,
                            caller_rank,
                            caller: format!("{}.{}", manifest.instance_id, route.field),
                            correlations: Arc::clone(correlations),
                        },
                    )
                } else {
                    None
                };
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
                        ack_leg,
                        cancel: worker_cancel,
                        reply_admission,
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
    pub(super) async fn bind_direct(
        &mut self,
        bus: crate::runtime::connection::Connection,
        instance: &str,
    ) -> crate::Result<()>
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
                binding: crate::runtime::transport::PortBinding::from_signature(signature),
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

    pub(super) fn ensure_open(&self) -> crate::Result<()> {
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(crate::runtime::connection::ConnectionError::Closed))?;
        if matches!(
            bus.terminal(),
            crate::runtime::connection::ConnectionTerminal::Open
        ) {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                crate::runtime::connection::ConnectionError::Closed
            ))
        }
    }

    pub(super) fn validate_stream_lifecycle(
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
                crate::runtime::transport::WireControl::End
                    | crate::runtime::transport::WireControl::Failed
            ) {
                terminal = true;
            }
        }
        Ok(terminal)
    }

    pub(super) fn validate_command_record(
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

    pub(super) fn validate_future_command_admission(
        &self,
        batch: &CollectedInput,
    ) -> crate::Result<()>
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
    R::Inputs: TransportInputSet + crate::runtime::input::TransportInputSink,
{
    fn freeze(&mut self, _candidate: &HardwareInvocation) -> crate::Result<R::Inputs> {
        if self.stopped {
            return Err(anyhow::anyhow!(
                crate::runtime::connection::ConnectionError::Closed
            ));
        }
        self.ensure_open()?;
        self.last_input_receipts.clear();
        let mut input_receipts = BTreeMap::<(String, String, String), RuntimeInputReceipt>::new();
        let mut inputs = R::Inputs::empty();
        for (field, value) in std::mem::take(&mut self.managed_inputs) {
            <R::Inputs as crate::runtime::input::TransportInputSink>::restore_managed(
                &mut inputs,
                field,
                value,
            )?;
        }
        let managed_fields = <R::Inputs as crate::runtime::input::InputSet>::FIELDS
            .iter()
            .filter(|field| {
                matches!(
                    field.kind,
                    crate::runtime::input::InputKind::Read
                        | crate::runtime::input::InputKind::Request
                        | crate::runtime::input::InputKind::Operation
                )
            })
            .collect::<Vec<_>>();
        let activation_states = self.activation_states.as_ref().ok_or_else(|| {
            anyhow::anyhow!(TransportError::Transport(
                "managed activation state is not bound".to_owned(),
            ))
        });
        if managed_fields.is_empty() {
            <R::Inputs as TransportInputSet>::expire_transport_fields_at(
                &mut inputs,
                _candidate.context().now(),
            )?;
        } else {
            let activation_states = activation_states?;
            let activation_states = activation_states
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for field in managed_fields {
                if let Some(activation) = activation_states.get(field.name) {
                    let attempt_started = self
                        .observed_attempts
                        .insert(field.name, activation.attempt)
                        != Some(activation.attempt);
                    <R::Inputs as crate::runtime::input::TransportInputSink>::select_managed(
                        &mut inputs,
                        field.name,
                        activation.key.clone().into_value(),
                        attempt_started,
                    )?;
                } else {
                    self.observed_attempts.remove(field.name);
                    <R::Inputs as crate::runtime::input::TransportInputSink>::retire_managed(
                        &mut inputs,
                        field.name,
                    )?;
                }
            }
            drop(activation_states);
            <R::Inputs as TransportInputSet>::expire_transport_fields_at(
                &mut inputs,
                _candidate.context().now(),
            )?;
        }
        if let Some(queue) = &self.operation_completions {
            let completions = {
                let mut queue = match queue.lock() {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
                std::mem::take(&mut *queue)
            };
            for completion in completions {
                <R::Inputs as crate::runtime::input::TransportInputSink>::set_operation(
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
                        <R::Inputs as crate::runtime::input::TransportInputSink>::set_read(
                            &mut inputs,
                            field,
                            key,
                            result,
                            None,
                        )?
                    }
                    ExchangeCompletion::Request { field, key, result } => {
                        <R::Inputs as crate::runtime::input::TransportInputSink>::set_request(
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
                queue.drain(_candidate.input_boundary())
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
                    .publisher()
                    .filter(|source| !source.is_empty())
                else {
                    continue;
                };
                let key = (
                    subscription.field.to_owned(),
                    source.to_owned(),
                    subscription.binding.name.clone(),
                );
                let receipt = input_receipts
                    .entry(key)
                    .or_insert_with(|| RuntimeInputReceipt {
                        input: subscription.field.to_owned(),
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
        let current_boundary = _candidate.input_boundary();
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

    fn retain(&mut self, mut inputs: R::Inputs) -> crate::Result<()> {
        self.managed_inputs.clear();
        for field in <R::Inputs as crate::runtime::input::InputSet>::FIELDS
            .iter()
            .filter(|field| {
                matches!(
                    field.kind,
                    crate::runtime::input::InputKind::Read
                        | crate::runtime::input::InputKind::Request
                        | crate::runtime::input::InputKind::Operation
                )
            })
        {
            let value = <R::Inputs as crate::runtime::input::TransportInputSink>::take_managed(
                &mut inputs,
                field.name,
            )?;
            self.managed_inputs.insert(field.name, value);
        }
        Ok(())
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
        self.managed_inputs.clear();
        self.observed_attempts.clear();
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
        self.managed_inputs.clear();
        self.observed_attempts.clear();
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
        if self.bus.is_none() {
            return Err(anyhow::anyhow!(TransportError::Transport(
                "Runtime input subscriptions are not bound after reset".to_owned(),
            )));
        }
        Ok(())
    }
}

pub(super) const MAX_COMMAND_HIGH_WATERMARKS: usize = 4096;

pub(super) const MAX_EXTERNAL_COMMANDS_PER_CUT: usize = 64;
