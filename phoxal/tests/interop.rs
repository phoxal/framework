//! Runtime-to-runtime interop: two runner-driven runtimes, a `Producer` and a
//! `Consumer`, exchange a typed contract over ONE shared in-process [`Bus`] via the
//! [`run_with_bus`] embedding seam.
//!
//! Everything below `run`/`run_with` (the macro-generated declarations, the runner
//! step loop, the typed publish/subscribe path, the bus codec + metadata) is
//! exercised end-to-end here — the first test that crosses a runtime boundary on a
//! live bus, rather than driving the bus or a single runtime in isolation.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use phoxal::bus::{Bus, BusConfig};
use phoxal::prelude::*;
use phoxal::runtime::{ParticipantLaunch, RealClock, run_with_bus};
use phoxal_api::y2026_1 as api;
use serial_test::serial;

static RECEIVED: AtomicU64 = AtomicU64::new(0);
static LAST_LINEAR_BITS: AtomicU32 = AtomicU32::new(0);

const TARGET_LINEAR_MPS: f32 = 0.5;

/// Publishes a fixed `drive/target` every step.
#[derive(phoxal::Runtime)]
#[phoxal(id = "producer", api = y2026_1)]
struct Producer {
    target: Publisher<api::drive::Target>,
}

#[phoxal::runtime]
impl Producer {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            target: ctx.publisher(api::topic::new().drive().target()).await?,
        })
    }

    #[step(hz = 50)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        self.target
            .publish_at(
                step.time(),
                api::drive::Target {
                    linear_x_mps: TARGET_LINEAR_MPS,
                    angular_z_radps: 0.0,
                    curvature_limit_radpm: None,
                },
            )
            .await?;
        Ok(())
    }
}

/// Subscribes `drive/target` and records what it receives into shared statics so
/// the test can assert delivery after both runtimes have shut down. It plays the
/// OWNER/reader of the `drive/target` command (the side that subscribes a command),
/// so it acquires the topic through the owner (`internal`) builder.
#[derive(phoxal::Runtime)]
#[phoxal(id = "consumer", api = y2026_1)]
struct Consumer {
    target: Subscriber<api::drive::Target>,
}

#[phoxal::runtime]
impl Consumer {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            target: ctx
                .subscribe(api::topic::internal::new().drive().target())
                .subscriber()
                .await?,
        })
    }

    #[step(hz = 50)]
    async fn step(&mut self, _step: StepContext) -> Result<()> {
        while let Some(received) = self.target.try_recv() {
            RECEIVED.fetch_add(1, Ordering::Relaxed);
            LAST_LINEAR_BITS.store(received.body.linear_x_mps.to_bits(), Ordering::Relaxed);
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn two_runtimes_exchange_a_contract_on_one_bus() {
    // One shared session: both participants publish/subscribe under the same root,
    // so the producer's puts reach the consumer's subscription (proven single-session
    // local delivery). Separate `run_with` calls would each open an isolated,
    // multicast-off session that could not discover the other.
    let bus = Bus::open(BusConfig::in_process("dev", "robot"))
        .await
        .expect("open shared bus");

    // Run both runtimes concurrently on the shared bus; each stops after a short
    // wall-clock window (enough for the 50 Hz producer to emit many samples).
    let producer = run_with_bus::<Producer, _, _>(
        &bus,
        ParticipantLaunch::local("producer-1", "robot"),
        RealClock::new(),
        async { tokio::time::sleep(Duration::from_millis(500)).await },
    );
    let consumer = run_with_bus::<Consumer, _, _>(
        &bus,
        ParticipantLaunch::local("consumer-1", "robot"),
        RealClock::new(),
        async { tokio::time::sleep(Duration::from_millis(500)).await },
    );

    let (producer_result, consumer_result) = tokio::join!(producer, consumer);
    producer_result.expect("producer ran cleanly");
    consumer_result.expect("consumer ran cleanly");

    bus.close().await.expect("close shared bus");

    assert!(
        RECEIVED.load(Ordering::Relaxed) > 0,
        "the consumer should have received the producer's targets over the shared bus"
    );
    let last = f32::from_bits(LAST_LINEAR_BITS.load(Ordering::Relaxed));
    assert!(
        (last - TARGET_LINEAR_MPS).abs() < 1e-6,
        "the received value should round-trip the published one (got {last})"
    );
}
