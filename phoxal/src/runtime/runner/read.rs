//! Owned immutable Read views and their schedule-independent query workers.

use super::{RuntimeLaunchManifest, validate_read_request_metadata};
use crate::runtime::{
    StepContext,
    connection::Connection,
    outputs::{OutputField, read::ReadView},
    transport::{self, PreparedOutput, WireSample},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub(super) struct Snapshot {
    context: StepContext,
    views: BTreeMap<&'static str, Arc<ReadView>>,
}

impl Snapshot {
    pub(super) fn new(context: StepContext, views: Vec<ReadView>) -> crate::Result<Self> {
        let mut mapped = BTreeMap::new();
        for view in views {
            anyhow::ensure!(
                mapped.insert(view.field, Arc::new(view)).is_none(),
                "duplicate Read projection"
            );
        }
        Ok(Self {
            context,
            views: mapped,
        })
    }
}

#[derive(Default)]
pub(super) struct Views {
    current: Option<Arc<Snapshot>>,
    pinned: Option<(u64, Arc<Snapshot>)>,
    timeline: Option<String>,
    generation: Arc<()>,
}

impl Views {
    pub(super) fn commit(&mut self, snapshot: Snapshot) {
        self.current = Some(Arc::new(snapshot));
    }
    pub(super) fn set_timeline(&mut self, timeline: &str) {
        let initialized = self
            .timeline
            .is_none()
            .then(|| self.current.clone())
            .flatten();
        self.clear();
        self.current = initialized;
        self.timeline = Some(timeline.to_owned());
    }

    pub(super) fn pin(&mut self, timeline: &str, boundary: u64) -> crate::Result<()> {
        anyhow::ensure!(
            self.timeline.as_deref() == Some(timeline),
            "Read pin uses a retired timeline"
        );
        if let Some((previous, _)) = &self.pinned {
            if *previous == boundary {
                return Ok(());
            }
            anyhow::ensure!(
                previous.checked_add(1) == Some(boundary),
                "Read pin skipped or reversed a boundary"
            );
        } else {
            anyhow::ensure!(boundary <= 1, "initial Read pin skipped a boundary");
        }
        let current = self
            .current
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Read views not initialized"))?;
        self.pinned = Some((boundary, current));
        Ok(())
    }

    pub(super) fn clear(&mut self) {
        self.current = None;
        self.pinned = None;
        self.generation = Arc::new(());
    }
}

pub(super) type SharedViews = Arc<Mutex<Views>>;

pub(super) struct Worker {
    cancel: CancellationToken,
    expected: Arc<AtomicBool>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.expected.store(true, Ordering::Release);
        self.cancel.cancel();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Graph,
    Observer,
}

pub(super) async fn bind(
    bus: &Connection,
    instance: &str,
    manifest: &RuntimeLaunchManifest,
    field: &OutputField,
    views: SharedViews,
    timeout: Duration,
) -> crate::Result<Worker> {
    let signature = field
        .port_signature
        .ok_or_else(|| anyhow::anyhow!("Read field has no public descriptor"))?;
    let max_request_bytes = field
        .max_request_bytes
        .ok_or_else(|| anyhow::anyhow!("Read field has no request bound"))?;
    let callers: BTreeMap<_, _> = manifest
        .command_ranks(instance)?
        .into_iter()
        .filter_map(|((port, caller), rank)| (port == signature.name).then_some((caller, rank)))
        .collect();
    anyhow::ensure!(
        callers.len() <= 64,
        "Read endpoint exceeds its 64-caller reservation"
    );
    let cancel = CancellationToken::new();
    let expected = Arc::new(AtomicBool::new(false));
    let owner = Worker {
        cancel: cancel.clone(),
        expected: expected.clone(),
    };
    let hardware_busy = Arc::new(AtomicBool::new(false));
    for (lane, leg, capacity) in [
        (Lane::Graph, "read-request", callers.len().max(1)),
        (Lane::Observer, "request", 2),
    ] {
        let subscriber = bus
            .session()?
            .declare_subscriber(bus.full_key(&transport::port_key(instance, signature.name, leg)))
            .with(zenoh::handlers::FifoChannel::new(capacity))
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let reply_acks = if lane == Lane::Graph {
            Some(
                super::declare_execution_subscriber(
                    bus,
                    instance,
                    &format!("read-reply-ack/{}", signature.name),
                )
                .await?,
            )
        } else {
            None
        };
        let endpoint = Endpoint {
            bus: bus.clone(),
            instance: instance.to_owned(),
            field: field.name,
            signature,
            max_request_bytes,
            callers: callers.clone(),
            views: views.clone(),
            timeout,
            cancel: cancel.clone(),
            hardware_busy: hardware_busy.clone(),
            lane,
            reply_acks,
        };
        let worker_expected = expected.clone();
        let worker = tokio::spawn(async move {
            let result = super::enforce_process_boundary(endpoint.run(subscriber).await);
            if let Err(error) = result {
                panic!("immutable Read worker failed: {error:#}");
            }
            assert!(
                worker_expected.load(Ordering::Acquire),
                "Read worker exited unexpectedly"
            );
        });
        if let Err(worker) = bus.register_named_worker(
            format!("read-{instance}-{}-{leg}", field.name),
            expected.clone(),
            worker,
        ) {
            worker.abort();
            anyhow::bail!("Read worker could not be registered");
        }
    }
    Ok(owner)
}

struct Endpoint {
    bus: Connection,
    instance: String,
    field: &'static str,
    signature: crate::port::PortSignature,
    max_request_bytes: u64,
    callers: BTreeMap<String, u64>,
    views: SharedViews,
    timeout: Duration,
    cancel: CancellationToken,
    hardware_busy: Arc<AtomicBool>,
    lane: Lane,
    reply_acks: Option<super::ExecutionSubscriber>,
}

struct QueryPermit(Arc<AtomicBool>);
impl Drop for QueryPermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct ActiveQuery {
    generation: Arc<()>,
    sample: WireSample,
    context: StepContext,
    worker: tokio::task::JoinHandle<crate::Result<PreparedOutput>>,
    _permit: Option<QueryPermit>,
    timeout: Duration,
}

struct CachedReply {
    request: WireSample,
    output: PreparedOutput,
}

impl Endpoint {
    fn controlled(&self) -> bool {
        self.views
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .timeline
            .is_some()
    }

    fn view(&self, sample: &WireSample) -> crate::Result<(Arc<()>, StepContext, Arc<ReadView>)> {
        let views = self.views.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = if sample.metadata().execution_id.is_some() {
            anyhow::ensure!(
                self.lane == Lane::Graph
                    && sample.metadata().execution_id.as_deref()
                        == Some(self.bus.execution().to_string().as_str())
                    && sample.metadata().timeline_id == views.timeline,
                "Read execution or timeline is not admitted"
            );
            let (boundary, snapshot) = views
                .pinned
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Read views were not pinned"))?;
            anyhow::ensure!(
                sample.metadata().boundary == Some(*boundary),
                "Read request does not match the pinned boundary"
            );
            snapshot
        } else {
            anyhow::ensure!(
                self.lane != Lane::Graph || views.timeline.is_none(),
                "controlled Read request has no delivery identity"
            );
            views
                .current
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Read view was not initialized"))?
        };
        let view = snapshot
            .views
            .get(self.field)
            .ok_or_else(|| anyhow::anyhow!("Read projection missing"))?;
        Ok((
            Arc::clone(&views.generation),
            snapshot.context,
            Arc::clone(view),
        ))
    }

    async fn retire(&self, query: ActiveQuery) -> crate::Result<()> {
        match tokio::time::timeout(self.timeout, query.worker).await {
            Ok(result) => {
                result??;
                Ok(())
            }
            Err(_) => Err(anyhow::anyhow!(
                super::RunnerError::ProcessTerminationRequired {
                    field: self.field,
                    detail: "immutable Read worker did not retire within its grace".to_owned(),
                }
            )),
        }
    }

    async fn publish_reply(
        &self,
        mut output: PreparedOutput,
        sample: &WireSample,
        timeout: Duration,
    ) -> crate::Result<PreparedOutput> {
        let metadata = sample.metadata();
        let Some(execution) = metadata.execution_id.as_deref() else {
            output.publish_async(&self.bus, &self.instance).await?;
            return Ok(output);
        };
        let timeline = metadata
            .timeline_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Read timeline missing"))?;
        let boundary = metadata
            .boundary
            .ok_or_else(|| anyhow::anyhow!("Read boundary missing"))?;
        output.stamp_delivery_identity(execution, timeline, boundary, 0)?;
        let (port, direction, target, sequence, item, bytes) = output
            .delivery_receipt(0)
            .ok_or_else(|| anyhow::anyhow!("Read reply has no delivery identity"))?;
        output.publish_async(&self.bus, &self.instance).await?;
        let reply_acks = self
            .reply_acks
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("required Read has no reply admission channel"))?;
        tokio::time::timeout(timeout, async {
            loop {
                let ack: super::execution_wire::DeliveryAck = tokio::select! {
                    _ = self.cancel.cancelled() => anyhow::bail!("Read reply admission cancelled"),
                    ack = super::recv_execution(reply_acks) => ack?,
                };
                if ack.execution_id != execution
                    || ack.timeline_id != timeline
                    || ack.boundary != boundary
                    || ack.source != self.instance
                    || Some(&ack.target) != target.as_ref()
                    || ack.port != port
                    || ack.direction != direction
                    || ack.sequence != sequence
                    || ack.item != item
                    || ack.bytes != bytes
                {
                    continue;
                }
                anyhow::ensure!(
                    ack.admitted,
                    "Read reply receiver refused admission: {}",
                    ack.detail.unwrap_or_default()
                );
                return Ok::<_, anyhow::Error>(());
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("Read reply receiver admission timed out"))??;
        let identity = super::input::DeliveryQueue::new(
            1,
            self.max_request_bytes,
            crate::runtime::input::InputKind::Read,
        )
        .identity(
            sample,
            &format!("{}.{}", self.instance, self.field),
            self.signature.name,
            "request",
        )?;
        super::input::publish_delivery_ack(&self.bus, &identity, "delivery-ack", true, None)
            .await?;
        Ok(output)
    }

    async fn reject_required(&self, sample: &WireSample, detail: String) -> crate::Result<()> {
        let identity = super::input::DeliveryQueue::new(
            1,
            self.max_request_bytes,
            crate::runtime::input::InputKind::Read,
        )
        .identity(
            sample,
            &format!("{}.{}", self.instance, self.field),
            self.signature.name,
            "request",
        )?;
        super::input::publish_delivery_ack(
            &self.bus,
            &identity,
            "delivery-ack",
            false,
            Some(detail),
        )
        .await
    }

    async fn refuse(
        &self,
        sample: &WireSample,
        context: StepContext,
        control: transport::WireControl,
    ) -> crate::Result<()> {
        anyhow::ensure!(
            sample.metadata().execution_id.is_none(),
            "required Read cannot return a scheduling-dependent refusal"
        );
        PreparedOutput::read_refusal(
            self.signature,
            control,
            transport::reply_metadata_for_request(&self.instance, context, sample.metadata())?,
        )
        .publish_async(&self.bus, &self.instance)
        .await
    }

    async fn run(self, subscriber: super::input::RuntimeSubscription) -> crate::Result<()> {
        let binding = transport::PortBinding::from_signature(self.signature);
        let mut active: Option<ActiveQuery> = None;
        let mut deadline = tokio::time::Instant::now();
        let mut completed = BTreeMap::<String, CachedReply>::new();
        let mut generation = Arc::new(());
        loop {
            let current_generation = self
                .views
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .generation
                .clone();
            if !Arc::ptr_eq(&generation, &current_generation) {
                completed.clear();
                generation = current_generation;
            }
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => {
                    if let Some(query) = active.take() { self.retire(query).await?; }
                    return Ok(());
                }
                result = async { match active.as_mut() {
                    Some(query) => (&mut query.worker).await,
                    None => std::future::pending().await,
                }} => {
                    let query = active.take().ok_or_else(|| anyhow::anyhow!("Read worker completion has no owner"))?;
                    if !Arc::ptr_eq(&self.views.lock().unwrap_or_else(|e| e.into_inner()).generation, &query.generation) { continue; }
                    let output = match result? {
                        Ok(output) => output,
                        Err(error) => {
                            let control = if error.downcast_ref::<transport::TransportError>().is_some_and(|error| matches!(error, transport::TransportError::BodyTooLarge { .. })) {
                                transport::WireControl::Oversized
                            } else { transport::WireControl::Failed };
                            if query.sample.metadata().execution_id.is_some() {
                                self.reject_required(&query.sample, error.to_string()).await?;
                            } else {
                                self.refuse(&query.sample, query.context, control).await?;
                            }
                            continue;
                        }
                    };
                    let output = match self.publish_reply(output, &query.sample, query.timeout).await {
                        Ok(output) => output,
                        Err(error) if query.sample.metadata().execution_id.is_some() => {
                            self.reject_required(&query.sample, error.to_string()).await?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    let caller = query.sample.metadata().caller.clone().ok_or_else(|| anyhow::anyhow!("Read caller missing"))?;
                    completed.insert(caller, CachedReply { request: query.sample, output });
                }
                _ = tokio::time::sleep_until(deadline), if active.is_some() => {
                    if let Some(query) = active.take() { self.retire(query).await?; }
                    anyhow::bail!("Read handler exceeded its execution deadline");
                }
                ignored = async { match &self.reply_acks {
                    Some(subscriber) => subscriber.recv_async().await,
                    None => std::future::pending().await,
                }} => {
                    ignored.map_err(|error| anyhow::anyhow!(error.to_string()))?;
                }
                received = subscriber.recv_async(), if active.is_none() || self.lane == Lane::Observer || !self.controlled() => {
                    let sample = WireSample::from_zenoh(received.map_err(|e| anyhow::anyhow!(e.to_string()))?)?;
                    let ingress = validate_read_request_metadata(&binding, &self.callers, &sample)?;
                    anyhow::ensure!((self.lane == Lane::Observer) == matches!(ingress, transport::CommandIngress::External { .. }), "Read request used the wrong admission lane");
                    let (current_generation, context, view) = match self.view(&sample) {
                        Ok(view) => view,
                        Err(error) if sample.metadata().execution_id.is_some() => {
                            self.reject_required(&sample, error.to_string()).await?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    let timeout = Duration::from_millis(sample.metadata().request_timeout_ms.unwrap_or(self.timeout.as_millis() as u64));
                    anyhow::ensure!(!timeout.is_zero(), "Read transfer timeout must be positive");
                    let caller = sample.metadata().caller.as_ref().ok_or_else(|| anyhow::anyhow!("Read caller missing"))?;
                    if let Some(previous) = completed.get(caller) {
                        let correlation = sample.metadata().ingress_sequence.or(sample.metadata().command_id);
                        let previous_correlation = previous.request.metadata().ingress_sequence.or(previous.request.metadata().command_id);
                        if correlation == previous_correlation {
                            anyhow::ensure!(sample.metadata() == previous.request.metadata() && sample.payload() == previous.request.payload(), "Read correlation reused with different request bytes");
                            self.publish_reply(previous.output.clone(), &sample, timeout).await?;
                            continue;
                        }
                        anyhow::ensure!(correlation > previous_correlation, "Read correlation is stale");
                    }
                    let required = sample.metadata().execution_id.is_some();
                    let permit = if required { None } else {
                        if self.hardware_busy.swap(true, Ordering::AcqRel) {
                            self.refuse(&sample, context, transport::WireControl::Busy).await?;
                            continue;
                        }
                        Some(QueryPermit(self.hardware_busy.clone()))
                    };
                    if sample.payload().len() as u64 > self.max_request_bytes {
                        self.refuse(&sample, context, transport::WireControl::Oversized).await?;
                        continue;
                    }
                    transport::validate_request(self.signature, &sample, self.max_request_bytes)?;
                    let source = self.instance.clone();
                    let request = sample.clone();
                    active = Some(ActiveQuery { generation: current_generation, context, sample,
                        worker: tokio::task::spawn_blocking(move || view.respond(&request, context, &source)),
                        _permit: permit, timeout });
                    deadline = tokio::time::Instant::now() + timeout;
                }
            }
        }
    }
}
