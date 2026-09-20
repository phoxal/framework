//! Accepted output publication, managed work, and immutable read projections.
use super::exchange::{
    AcceptedActivation, ActivationStateMap, CorrelationMap, ExchangeCompletion,
    ExchangeCompletionQueue, ExchangeKind, ExpiredCorrelationSet, MAX_EXPIRED_CORRELATIONS,
    OperationQueue, PendingCorrelation, not_sent_completion,
};
use super::read;
use super::{
    InputDirection, OutputSink, ResolvedInputRoute, RuntimeActuation, RuntimeDeliveryReceipt,
    RuntimeLaunchManifest, RuntimeProductReceipt, operation_error,
};
use crate::runtime::ExecutionTime;
use crate::runtime::core::{AcceptedInvocation, OutputAdmission, RegisteredRuntime};
use crate::runtime::input::{
    InputSet, OperationCompletionRecord, OperationInputError, ReadError, RequestError,
    TransportInputSet, TransportValue,
};
use crate::runtime::operation::{ManagedOperation, OperationOutcome, OperationPolicy};
use crate::runtime::outputs::activation::ActivationKey;
use crate::runtime::outputs::{OperationWorker, OutputBindings, OutputSet, RuntimeWorkSink};
use crate::runtime::schedule::ScheduleError;
use crate::runtime::transport::{self, ChangeToken, PreparedOutput, TransportError};
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct ErasedManagedOperation {
    operation: ManagedOperation<u64, TransportValue, TransportValue, OperationWorker>,
    keys: BTreeMap<u64, TransportValue>,
    pending_key: Option<u64>,
}

/// One output reservation owns both encoded records and the staged side
/// effects that are dispatched only after the invocation has been accepted.
pub(super) struct OutputReservation {
    keys: BTreeMap<&'static str, Option<ActivationKey>>,
    records: Vec<PreparedOutput>,
    activations: Vec<StagedActivation>,
    views: Option<read::Snapshot>,
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
pub(super) struct ExecutionOutputAdapter<R> {
    bus: Option<crate::runtime::connection::Connection>,
    instance: Option<String>,
    context: Option<crate::runtime::StepContext>,
    pub(super) projections: Vec<PreparedOutput>,
    read_workers: Vec<read::Worker>,
    read_views: read::SharedViews,
    staged_views: Option<read::Snapshot>,
    activation_routes: BTreeMap<&'static str, ResolvedInputRoute>,
    pub(super) reply_callers: BTreeMap<(String, u64), String>,
    staged: Vec<StagedActivation>,
    active_keys: BTreeMap<&'static str, ActivationKey>,
    staged_keys: BTreeMap<&'static str, Option<ActivationKey>>,
    operations: BTreeMap<&'static str, ErasedManagedOperation>,
    correlations: Option<CorrelationMap>,
    expired_correlations: Option<ExpiredCorrelationSet>,
    operation_completions: Option<OperationQueue>,
    exchange_completions: Option<ExchangeCompletionQueue>,
    activation_states: Option<ActivationStateMap>,
    next_refresh_steps: BTreeMap<&'static str, u64>,
    last_state_values: BTreeMap<&'static str, ChangeToken>,
    next_command_id: u64,
    last_product_receipts: Vec<RuntimeProductReceipt>,
    last_delivery_receipts: Vec<RuntimeDeliveryReceipt>,
    last_actuations: Vec<RuntimeActuation>,
    delivery_context: Option<(u64, String)>,
    stopped: bool,
    _runtime: PhantomData<fn() -> R>,
}

impl<R> ExecutionOutputAdapter<R> {
    pub(super) fn unbound() -> Self {
        Self {
            bus: None,
            instance: None,
            context: None,
            projections: Vec::new(),
            read_workers: Vec::new(),
            read_views: Arc::new(Mutex::new(read::Views::default())),
            staged_views: None,
            activation_routes: BTreeMap::new(),
            reply_callers: BTreeMap::new(),
            staged: Vec::new(),
            active_keys: BTreeMap::new(),
            staged_keys: BTreeMap::new(),
            operations: BTreeMap::new(),
            correlations: None,
            expired_correlations: None,
            operation_completions: None,
            exchange_completions: None,
            activation_states: Some(Arc::new(Mutex::new(BTreeMap::new()))),
            next_refresh_steps: BTreeMap::new(),
            last_state_values: BTreeMap::new(),
            next_command_id: 1,
            last_product_receipts: Vec::new(),
            last_delivery_receipts: Vec::new(),
            last_actuations: Vec::new(),
            delivery_context: None,
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

    fn publish_records(
        &mut self,
        mut records: Vec<PreparedOutput>,
        initialized: bool,
    ) -> crate::Result<()>
    where
        R: RegisteredRuntime,
        R::Inputs: InputSet + TransportInputSet,
        R::Outputs: OutputSet,
    {
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(crate::runtime::connection::ConnectionError::Closed))?;
        self.last_delivery_receipts.clear();
        for record in &mut records {
            record.bind_reply_caller(&self.reply_callers)?;
        }
        if let Some((boundary, timeline_id)) = self.delivery_context.take() {
            let execution_id = bus.execution().to_string();
            let mut item_indices = BTreeMap::<(String, String, Option<String>), u32>::new();
            for record in &mut records {
                let key = record.delivery_route();
                let item = item_indices.entry(key).or_insert(0);
                let item_index = *item;
                *item = item_index.saturating_add(1);
                if initialized {
                    record.stamp_initialized_state(&execution_id, &timeline_id, item_index);
                } else {
                    record.stamp_delivery_identity(
                        &execution_id,
                        &timeline_id,
                        boundary,
                        item_index,
                    )?;
                }
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
        let mut receipts = empty_product_receipts::<R>()?;
        transport::publish_batch(bus, self.instance.as_deref().unwrap_or_default(), &records)?;
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
        Ok(())
    }

    pub(super) fn filter_state_projections(
        &mut self,
        context: crate::runtime::StepContext,
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
            if metadata.kind != crate::runtime::outputs::OutputKind::State {
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

    pub(super) async fn bind(
        &mut self,
        bus: crate::runtime::connection::Connection,
        instance: &str,
        manifest: &RuntimeLaunchManifest,
    ) -> crate::Result<()>
    where
        R: RegisteredRuntime,
        R::Inputs: TransportInputSet,
        R::Outputs: OutputSet,
    {
        let mut read_workers = Vec::new();
        for field in <R as OutputBindings>::FIELDS {
            if field.kind == crate::runtime::outputs::OutputKind::Read {
                read_workers.push(
                    read::bind(
                        &bus,
                        instance,
                        manifest,
                        field,
                        Arc::clone(&self.read_views),
                        R::SPEC.timeout.as_duration(),
                    )
                    .await?,
                );
            }
        }

        let mut activation_routes = BTreeMap::new();
        for field in <R as OutputBindings>::FIELDS {
            if field.kind != crate::runtime::outputs::OutputKind::Activate {
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
            if input.kind == crate::runtime::input::InputKind::Operation {
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
        self.reply_callers = manifest
            .command_ranks(instance)?
            .into_iter()
            .map(|((port, caller), rank)| ((port, rank), caller))
            .collect();
        self.bus = Some(bus);
        self.instance = Some(instance.to_owned());
        self.read_workers = read_workers;
        self.activation_routes = activation_routes;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn bind_direct(
        &mut self,
        bus: crate::runtime::connection::Connection,
        instance: &str,
    ) {
        self.bus = Some(bus);
        self.instance = Some(instance.to_owned());
    }

    fn ensure_open(&self) -> crate::Result<()> {
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
                if !pending.reply_admitted
                    && pending.deadline.is_some_and(|deadline| deadline <= now)
                {
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
                if self
                    .active_keys
                    .get(field)
                    .is_none_or(|active| !active.matches_value(&*key))
                {
                    continue;
                }
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
            if operation.operation.state() == crate::runtime::operation::OperationState::Idle
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
        self.poll_operations()
    }

    fn commit_activation_keys(
        &mut self,
        keys: BTreeMap<&'static str, Option<ActivationKey>>,
    ) -> crate::Result<()> {
        for (field, key) in keys {
            if let (Some(correlations), Some(expired)) =
                (&self.correlations, &self.expired_correlations)
            {
                let mut correlations = correlations
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let mut expired = expired.lock().unwrap_or_else(|error| error.into_inner());
                correlations.retain(|identity, _| {
                    if identity.0 == field {
                        expired.insert(identity.clone());
                        false
                    } else {
                        true
                    }
                });
                while expired.len() > MAX_EXPIRED_CORRELATIONS {
                    expired.pop_first();
                }
            }
            self.next_refresh_steps.remove(field);
            if let Some(key) = key {
                let activation_states = self.activation_states.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(TransportError::Transport(
                        "managed activation state is not bound".to_owned(),
                    ))
                })?;
                activation_states
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(
                        field,
                        AcceptedActivation {
                            key: key.clone(),
                            attempt: 0,
                        },
                    );
                self.active_keys.insert(field, key);
            } else {
                if let Some(activation_states) = &self.activation_states {
                    activation_states
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .remove(field);
                }
                self.active_keys.remove(field);
                if let Some(operation) = self.operations.get_mut(field) {
                    operation.operation.withdraw();
                    if let Some(pending) = operation.pending_key.take() {
                        operation.keys.remove(&pending);
                    }
                }
            }
        }
        Ok(())
    }

    fn dispatch_activations(
        &mut self,
        activations: Vec<StagedActivation>,
        controlled: bool,
    ) -> crate::Result<()> {
        for activation in activations {
            let field = activation.field;
            let activation_states = self.activation_states.as_ref().ok_or_else(|| {
                anyhow::anyhow!(TransportError::Transport(
                    "managed activation state is not bound".to_owned(),
                ))
            })?;
            let mut activation_states = activation_states
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let accepted = activation_states.get_mut(field).ok_or_else(|| {
                anyhow::anyhow!(TransportError::InvalidMetadata {
                    detail: format!("activation `{field}` has no accepted key"),
                })
            })?;
            accepted.attempt = accepted.attempt.checked_add(1).ok_or_else(|| {
                anyhow::anyhow!(TransportError::CommandCorrelation(format!(
                    "activation `{field}` attempt space exhausted"
                )))
            })?;
            drop(activation_states);
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
                    .submit(crate::runtime::Activation::new(id, input))
                    .map_err(|error| operation_error(field, error))?
                {
                    crate::runtime::operation::SubmitResult::Started => {}
                    crate::runtime::operation::SubmitResult::Pending => {
                        operation.pending_key = Some(id);
                    }
                    crate::runtime::operation::SubmitResult::ReplacedPending => {
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
                    // Controlled phases bound required transfers and process
                    // liveness. Waiting for a later logical invocation or a
                    // healthy pause must not consume a hardware reply timeout.
                    deadline: (!controlled).then_some(deadline),
                    reply_admitted: false,
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
        key: ActivationKey,
        request: TransportValue,
        worker: Option<OperationWorker>,
        timeout_ms: Option<u64>,
        refresh_every_steps: Option<u64>,
        cancel_grace_ms: Option<u64>,
        context: crate::runtime::StepContext,
    ) -> crate::Result<()> {
        if self.stopped {
            return Err(anyhow::anyhow!(
                crate::runtime::connection::ConnectionError::Closed
            ));
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
        let same_key = self
            .active_keys
            .get(field)
            .is_some_and(|active| *active == key);
        if same_key && refresh_every_steps.is_none() {
            return Ok(());
        }
        if !same_key {
            self.staged_keys.insert(field, Some(key.clone()));
        }
        let key = key.into_value();
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
        if let Some(every) = refresh_every_steps.filter(|_| same_key) {
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
            if same_key && correlations.keys().any(|(candidate, _)| candidate == field) {
                return Ok(());
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
            let mut metadata = transport::request_metadata(
                source,
                caller_identity,
                context,
                command_id,
                context.invocation_index().saturating_add(1),
                caller_rank,
            );
            metadata.request_timeout_ms = Some(timeout_ms);
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

    fn deactivate(&mut self, field: &'static str) -> crate::Result<()> {
        if self.active_keys.contains_key(field) {
            self.staged_keys.insert(field, None);
        }
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
            return Err(anyhow::anyhow!(
                crate::runtime::connection::ConnectionError::Closed
            ));
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
            keys: std::mem::take(&mut self.staged_keys),
            records,
            activations,
            views: self.staged_views.take(),
        })
    }
}

/// Close the declared roster even when a sample batch is empty, a state is
/// unchanged, a projection is not due, or a setpoint is withdrawn.
fn empty_product_receipts<R: RegisteredRuntime>()
-> crate::Result<BTreeMap<String, RuntimeProductReceipt>>
where
    R::Inputs: InputSet,
    R::Outputs: OutputSet,
{
    use crate::runtime::outputs::OutputKind;
    <R::Outputs as OutputSet>::FIELDS
        .iter()
        .chain(<R as OutputBindings>::FIELDS)
        .filter(|field| {
            !matches!(
                field.kind,
                OutputKind::Read | OutputKind::Activate | OutputKind::Operation
            )
        })
        .map(|field| {
            let port = if field.kind == OutputKind::Reply {
                field
                    .input
                    .and_then(transport::input_port_signature::<R::Inputs>)
                    .map(|signature| signature.name)
            } else {
                field.port
            }
            .ok_or_else(|| {
                anyhow::anyhow!("output `{}` has no generated product endpoint", field.name)
            })?;
            Ok((
                port.to_owned(),
                RuntimeProductReceipt {
                    port: port.to_owned(),
                    sequence: 0,
                    items: 0,
                    bytes: 0,
                },
            ))
        })
        .collect()
}

impl<R> OutputSink<R> for ExecutionOutputAdapter<R>
where
    R: RegisteredRuntime,
    R::Inputs: InputSet + TransportInputSet,
    R::Outputs: OutputSet,
{
    fn prepare(&mut self, context: &crate::runtime::StepContext) -> crate::Result<()> {
        self.ensure_open()?;
        self.context = Some(*context);
        Ok(())
    }

    fn set_read_timeline(&mut self, timeline: &str) -> crate::Result<()> {
        self.read_views
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_timeline(timeline);
        Ok(())
    }

    fn pin_read_views(&mut self, timeline: &str, boundary: u64) -> crate::Result<()> {
        self.read_views
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pin(timeline, boundary)
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
        context: &crate::runtime::StepContext,
    ) -> crate::Result<()> {
        self.ensure_open()?;
        self.projections.clear();
        let source = self.instance.as_deref().unwrap_or_default().to_owned();
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

    fn prepare_read_views(
        &mut self,
        service: Arc<R>,
        state: &R::State,
        context: crate::runtime::StepContext,
    ) -> crate::Result<()> {
        self.staged_views = Some(read::Snapshot::new(
            context,
            service.prepare_read_views(state)?,
        )?);
        Ok(())
    }

    fn commit_read_views(&mut self) -> crate::Result<()> {
        if let Some(views) = self.staged_views.take() {
            self.read_views
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .commit(views);
        }
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
        let context = crate::runtime::StepContext::first(now, R::SPEC.period);
        let source = self.instance.as_deref().unwrap_or_default().to_owned();
        let resolve_input_port = |field: &str| transport::input_port_signature::<R::Inputs>(field);
        self.projections.extend(service.encode_transport(
            state,
            context,
            &resolve_input_port,
            &source,
        )?);
        self.filter_state_projections(context, true)?;
        let records = std::mem::take(&mut self.projections);
        self.publish_records(records, true)?;
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
        self.commit_activation_keys(reservation.keys)?;
        // Correlations must exist before a fast peer can return its reply.
        self.dispatch_activations(reservation.activations, self.delivery_context.is_some())?;
        self.publish_records(reservation.records, false)?;
        if let Some(views) = reservation.views {
            self.read_views
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .commit(views);
        }
        Ok(())
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
        self.staged_views = None;
        self.read_views
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.staged.clear();
        self.staged_keys.clear();
        self.active_keys.clear();
        if let Some(activation_states) = &self.activation_states {
            activation_states
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
        }
        self.next_refresh_steps.clear();
        self.last_state_values.clear();
        self.last_product_receipts.clear();
        self.last_delivery_receipts.clear();
        self.last_actuations.clear();
        self.delivery_context = None;
        self.read_workers.clear();
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
        self.staged_views = None;
        self.read_views
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.staged.clear();
        self.staged_keys.clear();
        self.active_keys.clear();
        if let Some(activation_states) = &self.activation_states {
            activation_states
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
        }
        self.next_refresh_steps.clear();
        self.last_state_values.clear();
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
