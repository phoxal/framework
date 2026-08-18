//! The endpoint-kind publisher handles.
//!
//! Each handle wraps the same private `Outbox` publish path - encode, build
//! provenance, enqueue - and differs only in the endpoint marker it is bounded
//! by and the robot time that marker permits it to express.

use std::marker::PhantomData;

use crate::bus::abi::{Codec, MessagePack};
use crate::bus::contract::{
    DeliveryFamily, EndpointDescriptor, EventContract, SampleContract, SetpointContract,
    StateContract, StreamContract, WorldClockContract,
};
use crate::bus::error::{BusError, Result};
use crate::bus::handle::stamp::StepStamp;
use crate::bus::runtime_metrics::RuntimeMetricHandle;
use crate::bus::session::{BusHandle, OUTBOUND_CAPACITY};
use crate::bus::time::{CaptureStamp, TimeWindow};
use crate::bus::topic::{Publish, Topic};

/// The shared publish path: encode, build provenance, enqueue. Private, so the
/// only public way to reach it is through an endpoint-kind publisher.
struct Outbox<E> {
    bus: BusHandle,
    key: String,
    family: DeliveryFamily,
    metric: RuntimeMetricHandle,
    _endpoint: PhantomData<fn() -> E>,
}

// Manual (not `#[derive(Clone)]`) so cloning never spuriously requires
// `E: Clone` - every field it actually holds is `Clone` regardless of `E`. All
// real operations take `&self`, so a clone is just a second handle to the same
// publish key on the same session (the runner's `Arc<Self::Api>`
// snapshot-sharing relies on every `Api` field type being cheaply `Clone` this
// way).
impl<E> Clone for Outbox<E> {
    fn clone(&self) -> Self {
        Outbox {
            bus: self.bus.clone(),
            key: self.key.clone(),
            family: self.family,
            metric: self.metric.clone(),
            _endpoint: PhantomData,
        }
    }
}

impl<E: EndpointDescriptor> Outbox<E> {
    fn new(bus: BusHandle, topic: &Topic<Publish<E>>) -> Result<Self> {
        let topic_key = topic.publish_key()?;
        let family = E::KIND.delivery_family();
        let metric = bus
            .runtime_metrics()?
            .register_outbound(topic_key, outbound_capacity(family));
        let key = bus.full_key(topic_key);
        Ok(Outbox {
            bus,
            key,
            family,
            metric,
            _endpoint: PhantomData,
        })
    }

    /// Encode `body`, build the [`BusMetadata`](crate::bus::metadata::BusMetadata),
    /// and admit it to the family-specific outbound lane. Returns immediately;
    /// no publisher path blocks the step loop.
    fn emit(&self, produced_at: Option<TimeWindow>, body: E::Payload) -> Result<()> {
        let payload = MessagePack::encode(&body)?;
        let metadata = self.bus.metadata(produced_at)?;
        self.bus.enqueue(
            self.key.clone(),
            MessagePack::ID.encoding_string(),
            payload,
            metadata,
            self.family,
            self.metric.clone(),
        )
    }
}

const fn outbound_capacity(family: DeliveryFamily) -> usize {
    match family {
        DeliveryFamily::State | DeliveryFamily::Setpoint => 1,
        DeliveryFamily::Sample | DeliveryFamily::Stream => OUTBOUND_CAPACITY,
        DeliveryFamily::Query => 1,
    }
}

/// Publishes the owner's current state at a logical step.
///
/// The transport keeps only the newest pending state. Replacements are
/// reported by the bus metrics.
pub struct StatePublisher<E: StateContract>(Outbox<E>);

impl<E: StateContract> Clone for StatePublisher<E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<E: StateContract> StatePublisher<E> {
    #[doc(hidden)]
    pub fn new(bus: BusHandle, topic: &Topic<Publish<E>>) -> Result<Self> {
        Ok(Self(Outbox::new(bus, topic)?))
    }
}

/// Publishes a captured sensor observation.
///
/// Samples retain their capture stamp. The bounded ordered lane evicts its
/// oldest item on overflow and reports the loss through bus metrics.
pub struct SamplePublisher<E: SampleContract>(Outbox<E>);

impl<E: SampleContract> Clone for SamplePublisher<E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<E: SampleContract> SamplePublisher<E> {
    #[doc(hidden)]
    pub fn new(bus: BusHandle, topic: &Topic<Publish<E>>) -> Result<Self> {
        Ok(Self(Outbox::new(bus, topic)?))
    }
}

/// Sends newest-actionable intent to the endpoint owner.
///
/// Setpoints carry no robot timestamp. A newer pending value replaces the
/// older value and the replacement is reported by bus metrics.
pub struct SetpointPublisher<E: SetpointContract>(Outbox<E>);

impl<E: SetpointContract> Clone for SetpointPublisher<E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<E: SetpointContract> SetpointPublisher<E> {
    #[doc(hidden)]
    pub fn new(bus: BusHandle, topic: &Topic<Publish<E>>) -> Result<Self> {
        Ok(Self(Outbox::new(bus, topic)?))
    }
}

/// Publishes ordered stream chunks in the endpoint's declared direction.
///
/// Stream chunks carry no robot timestamp. Saturation is returned to the
/// caller, and receivers expose gaps or terminal failure.
pub struct StreamPublisher<E: StreamContract>(Outbox<E>);

impl<E: StreamContract> Clone for StreamPublisher<E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<E: StreamContract> StreamPublisher<E> {
    #[doc(hidden)]
    pub fn new(bus: BusHandle, topic: &Topic<Publish<E>>) -> Result<Self> {
        Ok(Self(Outbox::new(bus, topic)?))
    }
}

/// Publishes the framework's own world-clock contract at a logical step.
///
/// A near-twin of [`StatePublisher`] - same step-stamped publish path - kept
/// as its own type rather than folded into `StatePublisher` precisely so
/// `StatePublisher`'s bound can stay the precise `StateContract` (see that
/// type's docs).
///
/// There is no documented way for a participant to build one: publishing the
/// world clock is the job of the external client that drives the simulated
/// world, which mints this publisher through [`Self::mint`] on its own bus
/// handle. The type is re-exported from neither `phoxal::bus` nor
/// `phoxal::prelude`.
pub struct WorldClockPublisher<B: WorldClockContract>(Outbox<B>);

impl<B: WorldClockContract> Clone for WorldClockPublisher<B> {
    fn clone(&self) -> Self {
        WorldClockPublisher(self.0.clone())
    }
}

impl<E: StateContract> StatePublisher<E> {
    /// Publish `body` as the state this step produced.
    pub fn publish(&self, step: &impl StepStamp, body: E::Payload) -> Result<()> {
        self.0.emit(Some(TimeWindow::exact(step.instant())), body)
    }
}

impl<B: WorldClockContract> WorldClockPublisher<B> {
    /// Build the world-clock publisher over a topic.
    ///
    /// Called by the external client that drives the simulated world and owns
    /// its clock; no participant reaches it. See [`crate::bus::handle::stamp`]'s
    /// module docs for the boundary that leaves this `pub`.
    #[doc(hidden)]
    pub fn mint(bus: BusHandle, topic: &Topic<Publish<B>>) -> Result<Self> {
        Ok(WorldClockPublisher(Outbox::new(bus, topic)?))
    }

    /// Publish `body` as the state this step produced.
    pub fn publish(&self, step: &impl StepStamp, body: B::Payload) -> Result<()> {
        self.0.emit(Some(TimeWindow::exact(step.instant())), body)
    }
}

impl<E: SampleContract> SamplePublisher<E> {
    /// Publish `body` as captured at `stamp`.
    pub fn publish(&self, stamp: CaptureStamp, body: E::Payload) -> Result<()> {
        self.0.emit(stamp.into_window(), body)
    }
}

impl<E: SetpointContract> SetpointPublisher<E> {
    /// Send `body` to the contract's owning service.
    pub fn send(&self, body: E::Payload) -> Result<()> {
        self.0.emit(None, body)
    }
}

impl<E: StreamContract> StreamPublisher<E> {
    /// Send one ordered stream chunk without blocking the step loop.
    pub fn send(&self, body: E::Payload) -> Result<()> {
        self.0.emit(None, body).map_err(|error| match error {
            BusError::Saturated { topic, .. } => BusError::WouldBlock { topic },
            error => error,
        })
    }
}

/// Publishes a discrete event produced by the owner at a logical step.
///
/// Events use the bounded ordered stream lane. Saturation is returned as
/// `WouldBlock`, and receivers expose producer gaps or terminal evidence.
pub struct EventPublisher<E: EventContract>(Outbox<E>);

impl<E: EventContract> Clone for EventPublisher<E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<E: EventContract> EventPublisher<E> {
    /// Build an event publisher over a generated endpoint topic.
    #[doc(hidden)]
    pub fn new(bus: BusHandle, topic: &Topic<Publish<E>>) -> Result<Self> {
        Ok(Self(Outbox::new(bus, topic)?))
    }

    /// Publish the event produced at this logical step.
    pub fn publish(&self, step: &impl StepStamp, body: E::Payload) -> Result<()> {
        self.0
            .emit(Some(TimeWindow::exact(step.instant())), body)
            .map_err(|error| match error {
                BusError::Saturated { topic, .. } => BusError::WouldBlock { topic },
                error => error,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::contract::{
        ApiFamily, EndpointDescriptor, EndpointKind, SampleContract, SetpointContract,
        SetpointDeliveryContract, StateContract, StreamContract,
    };
    use crate::bus::error::BusError;
    use crate::bus::handle::subscriber::SetpointReceiver;
    use crate::bus::runtime_metrics::RuntimeBufferKind;
    use crate::bus::session::{BusOwner, OUTBOUND_CAPACITY, OUTBOUND_MAX_BYTES};
    use crate::bus::test_support::{Target, TargetEndpoint, participant_config, step};
    use crate::bus::time::CaptureStamp;
    use crate::bus::topic::{Subscribe, Topic};
    use serial_test::serial;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct StreamChunk(Vec<u8>);

    enum StreamApi {}

    impl ApiFamily for StreamApi {
        const ID: &'static str = "stream-test";
    }

    struct StreamEndpoint;
    impl EndpointDescriptor for StreamEndpoint {
        type Api = StreamApi;
        type Payload = StreamChunk;
        const NAME: &'static str = "stream-test::Chunk";
        const FAMILY: &'static str = "stream-test";
        const CONTRACT: &'static str = "Chunk";
        const TOPIC: &'static str = "stream-test/chunk";
        const KIND: EndpointKind = EndpointKind::Stream;
    }

    impl StreamContract for StreamEndpoint {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct StateChunk(u16);

    struct StateEndpoint;
    impl EndpointDescriptor for StateEndpoint {
        type Api = StreamApi;
        type Payload = StateChunk;
        const NAME: &'static str = "stream-test::State";
        const FAMILY: &'static str = "stream-test";
        const CONTRACT: &'static str = "State";
        const TOPIC: &'static str = "stream-test/state";
        const KIND: EndpointKind = EndpointKind::State;
    }

    impl StateContract for StateEndpoint {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct SampleChunk(u16);

    struct SampleEndpoint;
    impl EndpointDescriptor for SampleEndpoint {
        type Api = StreamApi;
        type Payload = SampleChunk;
        const NAME: &'static str = "stream-test::Sample";
        const FAMILY: &'static str = "stream-test";
        const CONTRACT: &'static str = "Sample";
        const TOPIC: &'static str = "stream-test/sample";
        const KIND: EndpointKind = EndpointKind::Sample;
    }

    impl SampleContract for SampleEndpoint {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct SetpointChunk(u16);

    struct SetpointEndpoint;
    impl EndpointDescriptor for SetpointEndpoint {
        type Api = StreamApi;
        type Payload = SetpointChunk;
        const NAME: &'static str = "stream-test::Setpoint";
        const FAMILY: &'static str = "stream-test";
        const CONTRACT: &'static str = "Setpoint";
        const TOPIC: &'static str = "stream-test/setpoint";
        const KIND: EndpointKind = EndpointKind::Setpoint;
    }

    impl SetpointContract for SetpointEndpoint {}
    impl SetpointDeliveryContract for SetpointEndpoint {}

    /// A publish after close is a real loss, and the caller has to be able to
    /// see it: silently succeeding would let a participant believe it had
    /// reported state it never sent.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn publishing_on_a_closed_bus_reports_the_loss() {
        let (owner, bus) = BusOwner::open(participant_config("closed")).await.unwrap();
        let topic = Topic::<Publish<TargetEndpoint>>::new_static(TargetEndpoint::TOPIC);
        let publisher = StatePublisher::<TargetEndpoint>::new(bus.clone(), &topic).unwrap();
        owner.close().await;

        let error = publisher
            .publish(
                &step(1, 100),
                Target {
                    linear_x_mps: 0.9,
                    angular_z_radps: -0.1,
                },
            )
            .unwrap_err();
        assert!(matches!(error, BusError::Closed));
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn stream_saturation_is_reported_as_would_block() {
        let (owner, bus) = BusOwner::open(participant_config("stream-would-block"))
            .await
            .unwrap();
        let topic = Topic::<Publish<StreamEndpoint>>::new_static(StreamEndpoint::TOPIC);
        let publisher = StreamPublisher::<StreamEndpoint>::new(bus.clone(), &topic).unwrap();
        let error = publisher
            .send(StreamChunk(vec![0; OUTBOUND_MAX_BYTES + 1]))
            .expect_err("an oversized stream chunk must not be accepted");
        assert!(matches!(error, BusError::WouldBlock { .. }));
        owner.close().await;
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publisher_admission_enforces_each_delivery_family_and_records_evidence() {
        let (owner, bus) = BusOwner::open(participant_config("semantic-admission"))
            .await
            .unwrap();
        let state = StatePublisher::<StateEndpoint>::new(
            bus.clone(),
            &Topic::new_static(StateEndpoint::TOPIC),
        )
        .unwrap();
        let setpoint = SetpointPublisher::<SetpointEndpoint>::new(
            bus.clone(),
            &Topic::new_static(SetpointEndpoint::TOPIC),
        )
        .unwrap();
        let setpoint_receiver = SetpointReceiver::new(
            &bus,
            &Topic::<Subscribe<SetpointEndpoint>>::new_static(SetpointEndpoint::TOPIC),
        )
        .await
        .unwrap();
        let sample = SamplePublisher::<SampleEndpoint>::new(
            bus.clone(),
            &Topic::new_static(SampleEndpoint::TOPIC),
        )
        .unwrap();
        let pause = bus
            .test_pause_outbound_drain()
            .await
            .expect("test drain can be held before admission");
        let stream = StreamPublisher::<StreamEndpoint>::new(
            bus.clone(),
            &Topic::new_static(StreamEndpoint::TOPIC),
        )
        .unwrap();

        for value in 0..3 {
            state
                .publish(&step(1, value), StateChunk(value as u16))
                .unwrap();
        }
        for value in 0..3 {
            setpoint.send(SetpointChunk(value)).unwrap();
        }
        for value in 0..=OUTBOUND_CAPACITY {
            sample
                .publish(CaptureStamp::Untranslated, SampleChunk(value as u16))
                .unwrap();
        }
        for value in 0..OUTBOUND_CAPACITY {
            stream.send(StreamChunk(vec![value as u8])).unwrap();
        }
        assert!(matches!(
            stream.send(StreamChunk(vec![0xff])).unwrap_err(),
            BusError::WouldBlock { .. }
        ));

        let full_stream_key = bus.full_key(StreamEndpoint::TOPIC);
        let positions: Vec<_> = bus
            .test_queued_stream_metadata(&full_stream_key)
            .into_iter()
            .map(|metadata| {
                metadata
                    .stream_position
                    .expect("an accepted stream has a position")
                    .sequence
            })
            .collect();
        assert_eq!(positions, (0..OUTBOUND_CAPACITY as u64).collect::<Vec<_>>());

        let rows = bus.take_runtime_metrics().unwrap();
        let row = |topic: &str| {
            rows.iter()
                .find(|row| {
                    row.key.buffer_kind == RuntimeBufferKind::Outbound
                        && row.key.topic.ends_with(topic)
                })
                .expect("publisher metric row")
        };
        assert_eq!(row(StateEndpoint::TOPIC).count, 3);
        assert_eq!(row(StateEndpoint::TOPIC).latest_overwrites, 2);
        assert_eq!(row(StateEndpoint::TOPIC).high_water_depth, 1);
        assert_eq!(row(SetpointEndpoint::TOPIC).count, 3);
        assert_eq!(row(SetpointEndpoint::TOPIC).latest_overwrites, 2);
        assert_eq!(row(SetpointEndpoint::TOPIC).high_water_depth, 1);
        assert_eq!(
            row(SampleEndpoint::TOPIC).count,
            OUTBOUND_CAPACITY as u64 + 1
        );
        assert_eq!(row(SampleEndpoint::TOPIC).bounded_evictions, 1);
        assert_eq!(
            row(SampleEndpoint::TOPIC).high_water_depth,
            OUTBOUND_CAPACITY as u64
        );
        assert_eq!(row(StreamEndpoint::TOPIC).count, OUTBOUND_CAPACITY as u64);
        assert_eq!(row(StreamEndpoint::TOPIC).drops, 1);
        assert_eq!(
            bus.health()
                .outbound_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            2,
            "one sample eviction and one refused stream are both live evidence"
        );

        drop(pause);
        let delivered =
            tokio::time::timeout(std::time::Duration::from_secs(2), setpoint_receiver.recv())
                .await
                .expect("the drain must deliver the coalesced setpoint")
                .expect("setpoint receive remains healthy");
        assert_eq!(delivered.body.0, 2);
        owner
            .close_until(tokio::time::Instant::now() + std::time::Duration::from_secs(10))
            .await;
    }
}
