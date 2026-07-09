//! F-runtime slice: the real runtime proof for the NEW authoring model
//! (`participant::api` + `participant::runner_v2`) - not just a trybuild
//! compile (`tests/trybuild/pass/api_v2_wall_follower.rs` proves the macros
//! expand; this proves the generated `ParticipantLifecycle` impl actually
//! DRIVES through a live bus via `run_with_bus_v2`).
//!
//! Mirrors `tests/interop.rs`'s "one shared in-process `Bus`, a companion
//! client on the same session" shape, but for the new model + the query
//! surface interop.rs never exercises:
//!
//! - `#[setup]` returns `(Self, Self::Api)` and builds a `Publisher`
//!   (asserted via a companion `Latest` on the same bus) plus two declared
//!   `Server` slots;
//! - `#[step]` publishes through `&mut Self::Api` every tick;
//! - an exclusive `#[server(api = lookup)]` answers a real query
//!   (`&mut Self::Api`, D16-serialized with `#[step]`);
//! - a concurrent `#[server_snapshot(api = submap)]` answers TWO real queries
//!   AT THE SAME TIME (`tokio::join!`), reading both a committed `Snapshot`
//!   and a read-only `&Self::Api` - the D3 concurrent-server proof this slice
//!   exists to land, run against the runner's actual `Arc<R::Api>` sharing
//!   (`runner_v2`'s module docs), not a direct trait-method call;
//! - `#[shutdown]` publishes through `&mut Self::Api` one last time, observed
//!   by the companion `Latest` after the runner returns.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use phoxal::bus::{DEFAULT_QUERY_TIMEOUT, Latest, OwnerCap, Querier};
use phoxal::participant::{ParticipantLaunch, RealClock};
use phoxal::prelude::*;
use phoxal::raw::{Bus, BusConfig, run_with_bus_v2};
use phoxal_api::y2026_1 as api;

static STEPS_OBSERVED: AtomicU64 = AtomicU64::new(0);
static NAMESPACE_SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh in-process namespace per test invocation, so concurrently-run
/// `#[tokio::test]`s never share a Zenoh in-process session (mirrors
/// `tests/managed_tasks.rs`'s `unique_namespace`, avoiding the need for
/// `tests/interop.rs`'s `#[serial]` + fixed `"dev"` namespace instead).
fn unique_namespace() -> String {
    let seq = NAMESPACE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("test/runner-v2/{}/{}", std::process::id(), seq)
}

#[derive(phoxal::Api)]
struct Api {
    target: Publisher<api::drive::Target>,
    lookup: Server<api::frame::LookupRequest, api::frame::LookupResponse>,
    submap: Server<api::map::SubmapRequest, api::map::SubmapResponse>,
}

/// The committed snapshot `#[server_snapshot]` reads: the step count observed
/// as of the last commit, so a concurrent query can prove it saw REAL
/// participant state, not a stub.
struct WallFollowerSnapshot {
    steps: u64,
}

#[phoxal::service(id = "runtime-proof-v2", config = ())]
struct WallFollower {
    steps: u64,
}

#[phoxal::behavior]
impl WallFollower {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self { steps: 0 },
            Self::Api {
                target: ctx.publisher(api::topic::new().drive().target()).await?,
                lookup: ctx.server(api::topic::new().frame().lookup()).await?,
                submap: ctx.server(api::topic::new().map().submap()).await?,
            },
        ))
    }

    #[step(hz = 200)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        self.steps += 1;
        STEPS_OBSERVED.store(self.steps, Ordering::Relaxed);
        api.target
            .publish_at(
                step.time(),
                api::drive::Target {
                    linear_x_mps: 0.5,
                    angular_z_radps: 0.0,
                    curvature_limit_radpm: None,
                },
            )
            .await?;
        Ok(())
    }

    #[server(api = lookup)]
    async fn lookup(
        &mut self,
        api: &mut Self::Api,
        request: api::frame::LookupRequest,
    ) -> ServerResult<api::frame::LookupResponse> {
        let _ = (&*api, &request);
        Ok(api::frame::LookupResponse { transform: None })
    }

    #[server_snapshot(api = submap)]
    async fn submap(
        state: Snapshot<WallFollowerSnapshot>,
        api: &Self::Api,
        request: api::map::SubmapRequest,
    ) -> ServerResult<api::map::SubmapResponse> {
        let _ = (api, request);
        Ok(api::map::SubmapResponse {
            // Encodes the committed step count into the reply so the test can
            // assert this handler really read a live `Snapshot`, not `()`.
            width: state.get().steps as u32,
            height: 0,
            resolution_m: 0.05,
            cells: Vec::new(),
        })
    }

    #[snapshot]
    fn snapshot(&self) -> WallFollowerSnapshot {
        WallFollowerSnapshot { steps: self.steps }
    }

    #[shutdown]
    async fn shutdown(&mut self, api: &mut Self::Api, ctx: ShutdownContext) -> Result<()> {
        let _ = ctx;
        api.target
            .publish_at(
                LogicalTime::new(0, 0),
                api::drive::Target {
                    linear_x_mps: 0.0,
                    angular_z_radps: 0.0,
                    curvature_limit_radpm: None,
                },
            )
            .await?;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_model_participant_runs_through_a_real_bus() {
    let bus = Bus::open(BusConfig::in_process(unique_namespace(), "robot"))
        .await
        .expect("open shared bus");

    // Companion "client" handles built directly against the same bus (the
    // low-level constructors a raw tool would use - not the `ctx.*` builders,
    // which are `#[setup]`-only). `drive/target` is a "command" contract
    // (client publishes, owner subscribes - `phoxal-api`'s api tree), and
    // `WallFollower` publishes it from the client side below, so the
    // companion reads it from the OWNER side, exactly like
    // `tests/interop.rs`'s `Consumer` does for the same contract.
    let target_latest = Latest::<api::drive::Target>::new(
        &bus,
        &api::topic::internal::new(OwnerCap::__mint())
            .drive()
            .target(),
    )
    .await
    .expect("subscribe target");
    let lookup_querier = Querier::<api::frame::LookupRequest, api::frame::LookupResponse>::new(
        bus.clone(),
        &api::topic::new().frame().lookup(),
        DEFAULT_QUERY_TIMEOUT,
    )
    .expect("build lookup querier");
    let submap_querier = Querier::<api::map::SubmapRequest, api::map::SubmapResponse>::new(
        bus.clone(),
        &api::topic::new().map().submap(),
        DEFAULT_QUERY_TIMEOUT,
    )
    .expect("build submap querier");

    let launch = ParticipantLaunch::local("wall-follower-v2-1", "robot");
    let runner = run_with_bus_v2::<WallFollower, _, _>(&bus, launch, RealClock::new(), async {
        tokio::time::sleep(Duration::from_millis(600)).await
    });

    let queries = async {
        // Give the 200 Hz step loop a head start so a committed snapshot and
        // at least one publish exist before querying.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let lookup_reply = lookup_querier
            .query(api::frame::LookupRequest {
                target_frame_id: "map".to_string(),
                source_frame_id: "base".to_string(),
                at_ns: None,
            })
            .await
            .expect("exclusive #[server] should answer over the real bus");
        assert_eq!(lookup_reply.transform, None);

        // Two queries issued AT THE SAME TIME: the concurrent
        // `#[server_snapshot]` path must serve both without deadlocking
        // against the still-running #[step] loop or the exclusive server
        // above (D3/D16).
        let (first, second) = tokio::join!(
            submap_querier.query(api::map::SubmapRequest {
                min_x_m: 0.0,
                min_y_m: 0.0,
                max_x_m: 1.0,
                max_y_m: 1.0,
            }),
            submap_querier.query(api::map::SubmapRequest {
                min_x_m: 0.0,
                min_y_m: 0.0,
                max_x_m: 1.0,
                max_y_m: 1.0,
            }),
        );
        let first = first.expect("first concurrent #[server_snapshot] query should answer");
        let second = second.expect("second concurrent #[server_snapshot] query should answer");
        assert!(
            first.width > 0 && second.width > 0,
            "the snapshot server should read a real committed step count from #[step] (got {} and {})",
            first.width,
            second.width
        );
    };

    let (runner_result, ()) = tokio::join!(runner, queries);
    runner_result.expect("participant ran cleanly");

    bus.close().await.expect("close shared bus");

    assert!(
        STEPS_OBSERVED.load(Ordering::Relaxed) > 0,
        "the #[step] loop should have run at least once"
    );

    // `publish_at` only enqueues (never blocks/awaits delivery - D35/D43e), so
    // poll briefly for the post-`#[shutdown]` zeroed target to actually
    // arrive rather than asserting immediately.
    let mut zeroed = None;
    for _ in 0..50 {
        if let Some(sample) = target_latest.latest() {
            if sample.linear_x_mps == 0.0 {
                zeroed = Some(sample);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let zeroed = zeroed.expect(
        "#[shutdown]'s `&mut Self::Api` publish should eventually be observed as the zeroed target",
    );
    assert_eq!(zeroed.angular_z_radps, 0.0);
}

// ---------------------------------------------------------------------------
// Subscriber + Latest under the owned/Arc-shared split (F-runtime review gap)
// ---------------------------------------------------------------------------

// The first test omits the two `Api` field kinds whose sharing semantics the
// reviewer flagged: `Subscriber` (a DESTRUCTIVE shared queue - two clones
// compete) and `Latest` as an INBOUND handle (a non-destructive `ArcSwapOption`
// read). This second test drives a participant that owns BOTH through the same
// owned-`api` / `Arc<Self::Api>`-snapshot split the runner applies, and proves
// the SAFE path: the `#[step]` loop (which holds `&mut Self::Api`) drains ALL
// samples on its `Subscriber` and reads its `Latest` correctly, WHILE a
// concurrent `#[server_snapshot]` runs against the shared `Arc<Self::Api>`
// clone - a snapshot server that reads committed `Snapshot` state and never
// `recv`s the `Subscriber`, so it steals nothing. This is the field-kind
// coverage the D3 proof above is missing.

static DRAIN_RECEIVED_TOTAL: AtomicU64 = AtomicU64::new(0);
static DRAIN_LAST_VOLTAGE_BITS: AtomicU64 = AtomicU64::new(0);

/// Number of `drive/target` commands the companion feeds the drainer; small
/// enough to sit well inside the default 32-deep `Subscriber` ring, so the
/// only way a sample goes missing is a competing consumer stealing it.
const DRAIN_COMMANDS: u32 = 10;
const DRAIN_VOLTAGE_V: f32 = 12.6;

#[derive(phoxal::Api)]
struct DrainApi {
    // Owner-side subscription of the `drive/target` COMMAND (client publishes,
    // owner subscribes - like `tests/interop.rs`'s `Consumer`). This is the
    // destructive shared-queue field: it is cloned into the snapshot `Arc`,
    // but only `#[step]` (below) ever `recv`s it.
    incoming: Subscriber<api::drive::Target>,
    // Client-side keep-last-1 of the `battery/state` STATE (owner publishes,
    // client subscribes). The non-destructive inbound field.
    battery: Latest<api::battery::State>,
    // The concurrent snapshot server, deliberately reading committed state
    // only.
    query: Server<api::map::SubmapRequest, api::map::SubmapResponse>,
}

/// Committed state: what the step loop has drained/observed so far, so the
/// concurrent snapshot server can report REAL participant state without
/// touching the live `Subscriber`.
struct DrainSnapshot {
    received: u32,
    last_voltage_bits: u32,
}

// Explicit `api = DrainApi`: this test module already has an `Api` struct (the
// D3 test above), which is what the bare-`Api` default would resolve to.
#[phoxal::service(id = "drain-proof-v2", config = (), api = DrainApi)]
struct Drainer {
    received: u32,
    last_voltage_bits: u32,
}

#[phoxal::behavior]
impl Drainer {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let cap = ctx.owner_capability();
        Ok((
            Self {
                received: 0,
                last_voltage_bits: 0,
            },
            Self::Api {
                incoming: ctx
                    .subscriber(api::topic::internal::new(cap).drive().target(), 32)
                    .await?,
                battery: ctx.latest(api::topic::new().battery().state()).await?,
                query: ctx.server(api::topic::new().map().submap()).await?,
            },
        ))
    }

    #[step(hz = 200)]
    async fn step(&mut self, api: &mut Self::Api, _step: StepContext) -> Result<()> {
        // Drain the shared-queue `Subscriber` from the exclusive `&mut` side.
        // Nothing else `recv`s it, so this must observe every published sample.
        while api.incoming.try_recv().is_some() {
            self.received += 1;
        }
        // Non-destructive `Latest` read: every step sees the current value.
        if let Some(state) = api.battery.latest() {
            self.last_voltage_bits = state.voltage_v.to_bits();
        }
        DRAIN_RECEIVED_TOTAL.store(u64::from(self.received), Ordering::Relaxed);
        DRAIN_LAST_VOLTAGE_BITS.store(u64::from(self.last_voltage_bits), Ordering::Relaxed);
        Ok(())
    }

    #[server_snapshot(api = query)]
    async fn query(
        state: Snapshot<DrainSnapshot>,
        api: &Self::Api,
        request: api::map::SubmapRequest,
    ) -> ServerResult<api::map::SubmapResponse> {
        // The SAFE snapshot-server contract: read committed `Snapshot` state,
        // NEVER `recv` `api.incoming` (which would steal samples from the
        // `#[step]` side sharing the one ring - see `Subscriber`'s rustdoc).
        // `api` is bound only to prove the read-only `&Self::Api` view is
        // available here; it is intentionally not drained.
        let _ = (api, request);
        Ok(api::map::SubmapResponse {
            width: state.get().received,
            height: state.get().last_voltage_bits,
            resolution_m: 0.05,
            cells: Vec::new(),
        })
    }

    #[snapshot]
    fn snapshot(&self) -> DrainSnapshot {
        DrainSnapshot {
            received: self.received,
            last_voltage_bits: self.last_voltage_bits,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscriber_and_latest_survive_the_owned_arc_split() {
    let bus = Bus::open(BusConfig::in_process(unique_namespace(), "robot"))
        .await
        .expect("open shared bus");

    // Companion "client" handles on the same bus. `drive/target` is a command
    // (client publishes), `battery/state` is a state (owner publishes), so the
    // companion takes the client side of the former and the owner side of the
    // latter - the mirror of what `Drainer` subscribes.
    let target_pub =
        Publisher::<api::drive::Target>::new(bus.clone(), &api::topic::new().drive().target())
            .expect("build target publisher");
    let battery_pub = Publisher::<api::battery::State>::new(
        bus.clone(),
        &api::topic::internal::new(OwnerCap::__mint())
            .battery()
            .state(),
    )
    .expect("build battery publisher");
    let query_querier = Querier::<api::map::SubmapRequest, api::map::SubmapResponse>::new(
        bus.clone(),
        &api::topic::new().map().submap(),
        DEFAULT_QUERY_TIMEOUT,
    )
    .expect("build submap querier");

    let launch = ParticipantLaunch::local("drain-proof-v2-1", "robot");
    let runner = run_with_bus_v2::<Drainer, _, _>(&bus, launch, RealClock::new(), async {
        tokio::time::sleep(Duration::from_millis(800)).await
    });

    let driver = async {
        // Let `#[setup]` build the subscription + latest before publishing, so
        // no sample is emitted before the drainer is listening.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // One battery state for the `Latest` field.
        battery_pub
            .publish_at(
                LogicalTime::new(0, 1),
                api::battery::State {
                    voltage_v: DRAIN_VOLTAGE_V,
                    current_a: 1.0,
                    charge_ratio: 0.9,
                },
            )
            .await
            .expect("publish battery state");

        // Feed exactly `DRAIN_COMMANDS` commands, spaced so the 200 Hz step
        // loop drains between them (well under the 32-deep ring), then query
        // the snapshot server CONCURRENTLY - it must answer from committed
        // state while the step loop keeps draining, stealing nothing.
        for i in 0..DRAIN_COMMANDS {
            target_pub
                .publish_at(
                    LogicalTime::new(0, u64::from(i) + 1),
                    api::drive::Target {
                        linear_x_mps: 1.0,
                        angular_z_radps: 0.0,
                        curvature_limit_radpm: None,
                    },
                )
                .await
                .expect("publish target command");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Concurrent snapshot query mid-flight: proves the snapshot server runs
        // against the shared `Arc<Self::Api>` clone without deadlocking the
        // still-running step loop, and reports real drained state.
        let reply = query_querier
            .query(api::map::SubmapRequest {
                min_x_m: 0.0,
                min_y_m: 0.0,
                max_x_m: 1.0,
                max_y_m: 1.0,
            })
            .await
            .expect("snapshot server should answer concurrently");
        assert!(
            reply.width > 0,
            "the snapshot server should read a real drained count from committed state (got {})",
            reply.width
        );
    };

    let (runner_result, ()) = tokio::join!(runner, driver);
    runner_result.expect("drainer ran cleanly");

    bus.close().await.expect("close shared bus");

    // The core assertion: the `#[step]` side drained EVERY command. If the
    // concurrent snapshot server had `recv`'d the shared `Subscriber`, some
    // samples would have gone to it instead and this count would fall short -
    // the exact silent split the review flagged. It does not, because the
    // snapshot handler reads committed state only.
    assert_eq!(
        DRAIN_RECEIVED_TOTAL.load(Ordering::Relaxed),
        u64::from(DRAIN_COMMANDS),
        "the step loop must receive all commands on its Subscriber - none stolen by the concurrent snapshot server"
    );

    // The `Latest` field read the current value through the same owned `api`.
    assert_eq!(
        f32::from_bits(DRAIN_LAST_VOLTAGE_BITS.load(Ordering::Relaxed) as u32),
        DRAIN_VOLTAGE_V,
        "the step loop should read the published battery voltage through its Latest field"
    );
}
