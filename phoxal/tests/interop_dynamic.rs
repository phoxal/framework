//! Dynamic per-component topic interop: two runner-driven runtimes exchange a
//! contract over a *parameterized* topic key (D17/D38) on one shared in-process
//! bus.
//!
//! [`interop`](../interop.rs) covers a static topic; this locks down the dynamic
//! case — the load-bearing feature behind the `drive`/`ddsm115`/`odometry`
//! component topics. It proves that the macro's dynamic key builder produces the
//! *same* concrete key on both the publish and subscribe sides (here
//! `component/wheel-0/encoder/encoder/sample`), so a sample routes across the
//! runtime boundary.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use phoxal::bus::{Bus, BusConfig};
use phoxal::prelude::*;
use phoxal::runtime::{ParticipantLaunch, RealClock, run_with_bus};
use phoxal_api::y2026_1 as api;
use serial_test::serial;

static RECEIVED: AtomicU64 = AtomicU64::new(0);
static LAST_VELOCITY_BITS: AtomicU32 = AtomicU32::new(0);

const INSTANCE: &str = "wheel-0";
const CAPABILITY: &str = "encoder";
const SAMPLE_VELOCITY_RADPS: f32 = 2.5;

/// Publishes an `EncoderSample` on a dynamic per-component key every step.
#[derive(phoxal::Runtime)]
#[phoxal(id = "encoder-producer", api = y2026_1)]
struct EncoderProducer {
    encoder: Publisher<api::component::encoder::Sample>,
}

#[phoxal::runtime]
impl EncoderProducer {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            // The producer is the OWNER of the encoder sample (the encoder driver),
            // so it publishes the `state` topic through the owner (`internal`)
            // builder.
            encoder: ctx
                .publisher(
                    api::topic::internal::new()
                        .component(INSTANCE)
                        .encoder(CAPABILITY)
                        .sample(),
                )
                .await?,
        })
    }

    #[step(hz = 50)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        self.encoder
            .publish_at(
                step.time(),
                api::component::encoder::Sample {
                    position_rad: 1.0,
                    velocity_radps: SAMPLE_VELOCITY_RADPS,
                },
            )
            .await?;
        Ok(())
    }
}

/// Subscribes the *same* dynamic per-component key and records what it receives.
#[derive(phoxal::Runtime)]
#[phoxal(id = "encoder-consumer", api = y2026_1)]
struct EncoderConsumer {
    encoder: Subscriber<api::component::encoder::Sample>,
}

#[phoxal::runtime]
impl EncoderConsumer {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            encoder: ctx
                .subscribe(
                    api::topic::new()
                        .component(INSTANCE)
                        .encoder(CAPABILITY)
                        .sample(),
                )
                .subscriber()
                .await?,
        })
    }

    #[step(hz = 50)]
    async fn step(&mut self, _step: StepContext) -> Result<()> {
        while let Some(received) = self.encoder.try_recv() {
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

    let producer = run_with_bus::<EncoderProducer, _, _>(
        &bus,
        ParticipantLaunch::local("encoder-producer-1", "robot"),
        RealClock::new(),
        async { tokio::time::sleep(Duration::from_millis(500)).await },
    );
    let consumer = run_with_bus::<EncoderConsumer, _, _>(
        &bus,
        ParticipantLaunch::local("encoder-consumer-1", "robot"),
        RealClock::new(),
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
