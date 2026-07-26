//! Dynamic per-component topic interop: two runner-driven runtimes exchange a
//! contract over a *parameterized* topic key (D17/D38) on one shared in-process
//! bus.
//!
//! [`interop`](../interop.rs) covers a static topic; this locks down the dynamic
//! case — the load-bearing feature behind the `drive`/`ddsm115`/`odometry`
//! component topics. It proves that the macro's dynamic key builder produces the
//! *same* concrete key on both the publish and subscribe sides (here
//! `component/wheel-0/encoder/encoder/sample`), so a sample routes across the
//! participant boundary.

use phoxal::participant::ExecutionOrigin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use phoxal::api;
use phoxal::participant::ParticipantLaunch;
use phoxal::prelude::*;
use phoxal::raw::{Bus, BusConfig, run_with_bus};
use serial_test::serial;

static RECEIVED: AtomicU64 = AtomicU64::new(0);
static LAST_VELOCITY_BITS: AtomicU32 = AtomicU32::new(0);

const INSTANCE: &str = "wheel-0";
const CAPABILITY: &str = "encoder";
const SAMPLE_VELOCITY_RADPS: f32 = 2.5;

#[derive(phoxal::Api)]
struct EncoderProducerApi {
    encoder: MeasurementPublisher<api::component::encoder::Sample>,
}

/// Publishes an `EncoderSample` on a dynamic per-component key every step.
#[phoxal::service(id = "encoder-producer", config = (), api = EncoderProducerApi)]
struct EncoderProducer;

#[phoxal::behavior]
impl EncoderProducer {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let cap = ctx.owner_capability();
        Ok((
            Self,
            Self::Api {
                // The producer is the OWNER of the encoder sample (the encoder
                // driver), so it publishes the `state` topic through the owner
                // (`internal`) builder.
                encoder: ctx
                    .measurement_publisher(
                        api::topic::internal::new(cap)
                            .component(INSTANCE)
                            .encoder(CAPABILITY)
                            .sample(),
                    )
                    .await?,
            },
        ))
    }

    #[step(hz = 50)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        api.encoder.publish(
            CaptureStamp::exact(step.now()),
            api::component::encoder::Sample {
                position_rad: 1.0,
                velocity_radps: SAMPLE_VELOCITY_RADPS,
            },
        )?;
        Ok(())
    }
}

#[derive(phoxal::Api)]
struct EncoderConsumerApi {
    encoder: Subscriber<api::component::encoder::Sample>,
}

/// Subscribes the *same* dynamic per-component key and records what it receives.
#[phoxal::service(id = "encoder-consumer", config = (), api = EncoderConsumerApi)]
struct EncoderConsumer;

#[phoxal::behavior]
impl EncoderConsumer {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                encoder: ctx
                    .subscriber(
                        api::topic::new()
                            .component(INSTANCE)
                            .encoder(CAPABILITY)
                            .sample(),
                        32,
                    )
                    .await?,
            },
        ))
    }

    #[step(hz = 50)]
    async fn step(&mut self, api: &mut Self::Api, _step: StepContext) -> Result<()> {
        while let Some(received) = api.encoder.try_recv() {
            RECEIVED.fetch_add(1, Ordering::Relaxed);
            LAST_VELOCITY_BITS.store(received.body.velocity_radps.to_bits(), Ordering::Relaxed);
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn two_runtimes_exchange_a_dynamic_topic_on_one_bus() {
    let bus = Bus::open(BusConfig::in_process("dev", "robot"))
        .await
        .expect("open shared bus");

    let producer = run_with_bus::<EncoderProducer, _>(
        &bus,
        ParticipantLaunch::local("encoder-producer-1", "robot")
            .with_execution_origin(ExecutionOrigin::mint()),
        async { tokio::time::sleep(Duration::from_millis(500)).await },
    );
    let consumer = run_with_bus::<EncoderConsumer, _>(
        &bus,
        ParticipantLaunch::local("encoder-consumer-1", "robot")
            .with_execution_origin(ExecutionOrigin::mint()),
        async { tokio::time::sleep(Duration::from_millis(500)).await },
    );

    let (producer_result, consumer_result) = tokio::join!(producer, consumer);
    producer_result.expect("producer ran cleanly");
    consumer_result.expect("consumer ran cleanly");

    bus.close().await.expect("close shared bus");

    assert!(
        RECEIVED.load(Ordering::Relaxed) > 0,
        "the consumer should have received samples on the dynamic per-component key"
    );
    let last = f32::from_bits(LAST_VELOCITY_BITS.load(Ordering::Relaxed));
    assert!(
        (last - SAMPLE_VELOCITY_RADPS).abs() < 1e-6,
        "the received value should round-trip the published one (got {last})"
    );
}
