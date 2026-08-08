//! The role-bounded publisher handles.
//!
//! Each handle wraps the same private `Outbox` publish path - encode, build
//! provenance, enqueue - and differs only in the contract marker it is bounded
//! by and the robot time that marker permits it to express.

use std::marker::PhantomData;

use crate::abi::{Codec, MessagePack};
use crate::contract::{
    CommandContract, ContractBody, DiagnosticContract, MeasurementContract, StateContract,
    StreamContract, WorldClockContract,
};
use crate::error::{BusError, MetadataProblem, Result};
use crate::handle::stamp::StepStamp;
use crate::runtime_metrics::RuntimeMetricHandle;
use crate::session::{BusHandle, OUTBOUND_CAPACITY};
use crate::time::{CaptureStamp, TimeWindow};
use crate::topic::{Publish, Topic};

/// The shared publish path: encode, build provenance, enqueue. Private, so the
/// only public way to reach it is through a role-bounded publisher.
struct Outbox<B> {
    bus: BusHandle,
    key: String,
    metric: RuntimeMetricHandle,
    _body: PhantomData<fn() -> B>,
}

// Manual (not `#[derive(Clone)]`) so cloning never spuriously requires
// `B: Clone` - every field it actually holds is `Clone` regardless of `B`. All
// real operations take `&self`, so a clone is just a second handle to the same
// publish key on the same session (the runner's `Arc<Self::Api>`
// snapshot-sharing relies on every `Api` field type being cheaply `Clone` this
// way).
impl<B> Clone for Outbox<B> {
    fn clone(&self) -> Self {
        Outbox {
            bus: self.bus.clone(),
            key: self.key.clone(),
            metric: self.metric.clone(),
            _body: PhantomData,
        }
    }
}

impl<B: ContractBody> Outbox<B> {
    fn new(bus: BusHandle, topic: &Topic<Publish<B>>) -> Result<Self> {
        let topic_key = topic.publish_key()?;
        let metric = bus
            .runtime_metrics()?
            .register_outbound(topic_key, OUTBOUND_CAPACITY);
        let key = bus.full_key(topic_key);
        Ok(Outbox {
            bus,
            key,
            metric,
            _body: PhantomData,
        })
    }

    /// Encode `body`, build the [`BusMetadata`](crate::metadata::BusMetadata),
    /// and enqueue it. Returns immediately. A saturated outbound queue (sample
    /// or byte bound) returns [`BusError::Saturated`](crate::error::BusError) -
    /// the sample was dropped and `outbound_drops` bumped - so the caller can
    /// observe the loss; a closed session returns `Closed`.
    fn emit(&self, produced_at: Option<TimeWindow>, body: B) -> Result<()> {
        let payload = MessagePack::encode(&body)?;
        let metadata = self.bus.metadata(produced_at)?;
        let attachment = metadata
            .encode()
            .map_err(|e| crate::error::BusError::metadata(&self.key, MetadataProblem::Encode(e)))?;
        self.bus.enqueue(
            self.key.clone(),
            MessagePack::ID.encoding_string(),
            attachment,
            payload,
            self.metric.clone(),
        )
    }
}

macro_rules! role_publisher {
    ($name:ident, $bound:ident, $doc:literal) => {
        #[doc = $doc]
        ///
        /// The role marker is a bound on the *type*, not just on its methods,
        /// so naming the wrong publisher for a contract is rejected where the
        /// `Api` struct declares the field - the earliest and clearest place.
        pub struct $name<B: $bound>(Outbox<B>);

        impl<B: $bound> Clone for $name<B> {
            fn clone(&self) -> Self {
                $name(self.0.clone())
            }
        }

        impl<B: $bound> $name<B> {
            /// Build the handle over a topic.
            ///
            /// The author-facing path is the matching `ctx.*_publisher(...)`
            /// builder in `Participant::setup`. `pub` only because the
            /// generated api tree and the runner live in other crates; see
            /// [`crate::handle::stamp`]'s module docs for the full statement of
            /// what that does and does not close.
            #[doc(hidden)]
            pub fn new(bus: BusHandle, topic: &Topic<Publish<B>>) -> Result<Self> {
                Ok($name(Outbox::new(bus, topic)?))
            }
        }
    };
}

role_publisher!(
    StatePublisher,
    StateContract,
    "Publishes state at a logical step.\n\nThe step instant comes from a \
     framework-minted [`StepToken`](crate::handle::stamp::StepToken) or \
     [`WorldStepToken`](crate::handle::stamp::WorldStepToken), so a participant \
     cannot publish state at a time it did not reach. Non-blocking, so it is \
     safe to call from the step loop. The framework's own world-clock contract \
     is deliberately NOT a `StateContract` and so cannot be named here; see \
     [`WorldClockPublisher`]."
);

role_publisher!(
    MeasurementPublisher,
    MeasurementContract,
    "Publishes a sensor observation with its capture stamp.\n\nThe driver owns \
     mapping its device clock into robot time - including reset, drift, \
     wraparound, batching, and exposure-versus-readout semantics - and says so \
     honestly through [`CaptureStamp`], which can represent an untranslated \
     capture rather than inventing an instant."
);

role_publisher!(
    CommandPublisher,
    CommandContract,
    "Sends a command.\n\nA command is a request, not an observation: it \
     expresses no robot time. The owning service stamps its own observation and \
     applies the result at a logical step."
);

role_publisher!(
    StreamPublisher,
    StreamContract,
    "Publishes ordered stream chunks.\n\nThe bounded bus queue reports saturation as a typed error and close as `Closed`; a chunk is never silently discarded."
);

role_publisher!(
    DiagnosticPublisher,
    DiagnosticContract,
    "Publishes an output that describes the participant rather than the world \
     (health, logs, runtime evidence). It expresses no robot time."
);

/// Publishes the framework's own world-clock contract at a logical step.
///
/// A near-twin of [`StatePublisher`] - same step-stamped publish path - kept
/// as its own type rather than folded into `StatePublisher` precisely so
/// `StatePublisher`'s bound can stay the precise `StateContract` (see that
/// type's docs).
///
/// The documented way to build one is `SetupContext::world_clock_publisher` in
/// the `phoxal` crate, gated on the world-authority surface, and this type is
/// re-exported from neither `phoxal::bus` nor `phoxal::prelude`.
pub struct WorldClockPublisher<B: WorldClockContract>(Outbox<B>);

impl<B: WorldClockContract> Clone for WorldClockPublisher<B> {
    fn clone(&self) -> Self {
        WorldClockPublisher(self.0.clone())
    }
}

impl<B: StateContract> StatePublisher<B> {
    /// Publish `body` as the state this step produced.
    pub fn publish(&self, step: &impl StepStamp, body: B) -> Result<()> {
        self.0.emit(Some(TimeWindow::exact(step.instant())), body)
    }
}

impl<B: WorldClockContract> WorldClockPublisher<B> {
    /// Build the world-clock publisher over a topic.
    ///
    /// Callable only by `phoxal`'s simulator setup context; see
    /// [`crate::handle::stamp`]'s module docs for why it must nonetheless be
    /// `pub`.
    #[doc(hidden)]
    pub fn mint(bus: BusHandle, topic: &Topic<Publish<B>>) -> Result<Self> {
        Ok(WorldClockPublisher(Outbox::new(bus, topic)?))
    }

    /// Publish `body` as the state this step produced.
    pub fn publish(&self, step: &impl StepStamp, body: B) -> Result<()> {
        self.0.emit(Some(TimeWindow::exact(step.instant())), body)
    }
}

impl<B: MeasurementContract> MeasurementPublisher<B> {
    /// Publish `body` as captured at `stamp`.
    pub fn publish(&self, stamp: CaptureStamp, body: B) -> Result<()> {
        self.0.emit(stamp.into_window(), body)
    }
}

impl<B: CommandContract> CommandPublisher<B> {
    /// Send `body` to the contract's owning service.
    pub fn send(&self, body: B) -> Result<()> {
        self.0.emit(None, body)
    }
}

impl<B: StreamContract> StreamPublisher<B> {
    /// Send one ordered stream chunk without blocking the step loop.
    pub fn send(&self, body: B) -> Result<()> {
        self.0.emit(None, body).map_err(|error| match error {
            BusError::Saturated { topic, .. } => BusError::WouldBlock { topic },
            error => error,
        })
    }
}

impl<B: DiagnosticContract> DiagnosticPublisher<B> {
    /// Publish `body`.
    pub fn publish(&self, body: B) -> Result<()> {
        self.0.emit(None, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{ApiVersion, DeliveryFamily, StreamContract, TopicRole};
    use crate::error::BusError;
    use crate::session::{BusOwner, OUTBOUND_MAX_BYTES};
    use crate::test_support::{Target, participant_config, step};
    use crate::topic::Topic;
    use serial_test::serial;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct StreamChunk(Vec<u8>);

    enum StreamApi {}

    impl ApiVersion for StreamApi {
        const ID: &'static str = "stream-test";
    }

    impl ContractBody for StreamChunk {
        type Api = StreamApi;
        const NAME: &'static str = "stream-test::Chunk";
        const VERSION: &'static str = "stream-test";
        const CONTRACT: &'static str = "Chunk";
        const TOPIC: &'static str = "stream-test/chunk";
        const ROLE: TopicRole = TopicRole::Stream;
        const DELIVERY: DeliveryFamily = DeliveryFamily::Stream;
    }

    impl StreamContract for StreamChunk {}

    /// A publish after close is a real loss, and the caller has to be able to
    /// see it: silently succeeding would let a participant believe it had
    /// reported state it never sent.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn publishing_on_a_closed_bus_reports_the_loss() {
        let (owner, bus) = BusOwner::open(participant_config("closed")).await.unwrap();
        let topic = Topic::<Publish<Target>>::new_static(<Target as ContractBody>::TOPIC);
        let publisher = StatePublisher::<Target>::new(bus.clone(), &topic).unwrap();
        owner.close().await.unwrap();

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
        let topic = Topic::<Publish<StreamChunk>>::new_static(StreamChunk::TOPIC);
        let publisher = StreamPublisher::<StreamChunk>::new(bus.clone(), &topic).unwrap();
        let error = publisher
            .send(StreamChunk(vec![0; OUTBOUND_MAX_BYTES + 1]))
            .expect_err("an oversized stream chunk must not be accepted");
        assert!(matches!(error, BusError::WouldBlock { .. }));
        owner.close().await.unwrap();
    }
}
