//! Runner integration: a scheduled participant runs steps and then shuts down
//! cleanly, presence heartbeats track readiness, a slow `#[shutdown]` hook is
//! bounded by grace, `ClockMode::Simulation` steps track the live
//! `simulation/clock` feed, and the real runtime proof for the query surface -
//! not just a trybuild compile (`tests/trybuild/pass/wall_follower.rs`
//! proves the macros expand; this proves the generated `ParticipantLifecycle`
//! impl actually DRIVES through a live bus via `run_with_bus`).
//!
//! Mirrors `tests/interop.rs`'s "one shared in-process `Bus`, a companion
//! client on the same session" shape, but for the query surface interop.rs
//! never exercises:
//!
//! - `#[setup]` returns `(Self, Self::Api)` and builds a `Publisher`
//!   (asserted via a companion `Latest` on the same bus) plus two declared
//!   `Server` slots;
//! - `#[step]` publishes through `&mut Self::Api` every tick;
//! - an exclusive `#[server(api = lookup)]` answers a real query
//!   (`&mut Self::Api`, D16-serialized with `#[step]`);
//! - a concurrent `#[server_snapshot(api = submap)]` answers TWO real queries
//!   AT THE SAME TIME (`tokio::join!`), reading both a committed `Snapshot`
//!   and a read-only `&Self::Api` - the D3 concurrent-server proof, run
//!   against the runner's actual `Arc<R::Api>` sharing (`runner`'s module
//!   docs), not a direct trait-method call;
//! - `#[shutdown]` publishes through `&mut Self::Api` one last time, observed
//!   by the companion `Latest` after the runner returns.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use phoxal::bus::{DEFAULT_QUERY_TIMEOUT, Latest, LogicalTime, OwnerCap, Publisher, Querier};
use phoxal::participant::{ClockMode, ParticipantLaunch, RealClock, TestClock};
use phoxal::prelude::*;
use phoxal::raw::{Bus, BusConfig, run_with_bus};
use phoxal_api::y2026_1 as api;

static STEPS_OBSERVED: AtomicU64 = AtomicU64::new(0);
static NAMESPACE_SEQ: AtomicU64 = AtomicU64::new(0);
static COUNTER_STEPS: AtomicU64 = AtomicU64::new(0);
static SHUTDOWN_CALLED: AtomicBool = AtomicBool::new(false);
static SLOW_SHUTDOWN_COMPLETED: AtomicBool = AtomicBool::new(false);
static SIM_CLOCK_STEPS: AtomicU64 = AtomicU64::new(0);

/// A fresh in-process namespace per test invocation, so concurrently-run
/// `#[tokio::test]`s never share a Zenoh in-process session (mirrors
/// `tests/managed_tasks.rs`'s `unique_namespace`, avoiding the need for
/// `tests/interop.rs`'s `#[serial]` + fixed `"dev"` namespace instead).
fn unique_namespace(label: &str) -> String {
    let seq = NAMESPACE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("test/{label}/{}/{}", std::process::id(), seq)
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
    let bus = Bus::open(BusConfig::in_process(
        unique_namespace("runner-v2"),
        "robot",
    ))
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
    let runner = run_with_bus::<WallFollower, _, _>(&bus, launch, RealClock::new(), async {
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
    let bus = Bus::open(BusConfig::in_process(
        unique_namespace("drain-proof-v2"),
        "robot",
    ))
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
    let runner = run_with_bus::<Drainer, _, _>(&bus, launch, RealClock::new(), async {
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

// ---------------------------------------------------------------------------
// Runner-level behavior: shutdown/heartbeat/grace/simulation-clock proofs
// ---------------------------------------------------------------------------

#[derive(phoxal::Api)]
struct CounterApi {
    target: Publisher<api::drive::Target>,
}

#[phoxal::service(id = "counter", config = (), api = CounterApi)]
struct Counter;

#[phoxal::behavior]
impl Counter {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                target: ctx.publisher(api::topic::new().drive().target()).await?,
            },
        ))
    }

    #[step(hz = 200)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        COUNTER_STEPS.fetch_add(1, Ordering::Relaxed);
        api.target
            .publish_at(
                step.time(),
                api::drive::Target {
                    linear_x_mps: 0.0,
                    angular_z_radps: 0.0,
                    curvature_limit_radpm: None,
                },
            )
            .await?;
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _api: &mut Self::Api) -> Result<()> {
        SHUTDOWN_CALLED.store(true, Ordering::Relaxed);
        Ok(())
    }
}

#[phoxal::service(id = "idle-presence", config = (), api = ())]
struct IdlePresence;

#[phoxal::behavior]
impl IdlePresence {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }
}

#[phoxal::service(id = "slow-shutdown", config = (), api = ())]
struct SlowShutdown;

#[phoxal::behavior]
impl SlowShutdown {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[shutdown]
    async fn shutdown(&mut self, _api: &mut Self::Api, ctx: ShutdownContext) -> Result<()> {
        // Park far longer than the runner's grace. If the grace were not
        // enforced, `run_with` would block here for 60s.
        let _ = ctx.grace();
        tokio::time::sleep(Duration::from_secs(60)).await;
        SLOW_SHUTDOWN_COMPLETED.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// Acceptance fixture (P6-3): a `#[step]` participant with no other IO, used
/// to prove `#[step]` ticks under `ClockMode::Simulation` release only as the
/// live `simulation/clock` feed advances (see `simulation_mode_step_advances_only_with_the_clock_feed`).
#[phoxal::service(id = "sim-clock-stepper", config = (), api = ())]
struct SimClockStepper;

#[phoxal::behavior]
impl SimClockStepper {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[step(hz = 1000)]
    async fn step(&mut self, _api: &mut Self::Api, _step: StepContext) -> Result<()> {
        SIM_CLOCK_STEPS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[phoxal::driver(id = "component-driver", config = (), api = ())]
struct ComponentDriver;

#[phoxal::behavior]
impl ComponentDriver {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let _component = ctx.component()?;
        Ok((Self, ()))
    }
}

#[phoxal::simulator(id = "world-simulator", config = (), api = ())]
struct WorldSimulator;

#[phoxal::behavior]
impl WorldSimulator {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[step(hz = 20)]
    async fn step(&mut self, _api: &mut Self::Api, step: StepContext) -> Result<()> {
        let _ = step.time();
        Ok(())
    }
}

#[phoxal::tool(id = "robot-inspector", config = ())]
struct RobotInspector;

#[phoxal::behavior]
impl RobotInspector {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let _ = ctx.robot();
        Ok((Self, ()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runner_runs_steps_then_shuts_down_cleanly() {
    let launch = ParticipantLaunch::local("counter-1", "robot");
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    phoxal::participant::run_with::<Counter, _, _>(launch, RealClock::new(), shutdown)
        .await
        .expect("runner should complete cleanly");

    assert!(
        COUNTER_STEPS.load(Ordering::Relaxed) > 0,
        "the scheduled step should have run at least once"
    );
    assert!(
        SHUTDOWN_CALLED.load(Ordering::Relaxed),
        "the #[shutdown] hook should have run"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runner_publishes_presence_heartbeats_from_idle_loop() {
    let participant_id = "idle-presence-1";
    let namespace = unique_namespace("heartbeat");
    let mut bus_config = BusConfig::in_process(namespace.clone(), "robot");
    bus_config.participant = participant_id.to_string();
    let bus = Bus::open(bus_config).await.expect("bus should open");
    let heartbeat_topic = api::topic::internal::new(OwnerCap::__mint())
        .presence()
        .heartbeat();
    let heartbeats =
        phoxal::bus::Subscriber::<api::presence::Heartbeat>::new(&bus, &heartbeat_topic, 16)
            .await
            .expect("heartbeat subscriber should attach");

    let mut launch = ParticipantLaunch::local(participant_id, "robot");
    launch.namespace = namespace;
    let runner = run_with_bus::<IdlePresence, _, _>(&bus, launch, RealClock::new(), async {
        tokio::time::sleep(Duration::from_millis(2200)).await;
    });
    let collector = async {
        let mut readiness = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(3200);
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining.min(Duration::from_millis(250)), heartbeats.recv())
                .await
            {
                Ok(Ok(received)) if received.body.participant == participant_id => {
                    readiness.push(received.body.readiness);
                    if readiness.contains(&api::presence::Readiness::Initializing)
                        && readiness.contains(&api::presence::Readiness::Degraded)
                        && readiness
                            .iter()
                            .filter(|state| **state == api::presence::Readiness::Ready)
                            .count()
                            >= 2
                    {
                        break;
                    }
                }
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {}
            }
        }
        readiness
    };

    let (run_result, readiness) = tokio::join!(runner, collector);
    run_result.expect("runner should complete cleanly");
    bus.close().await.expect("bus should close");

    assert!(
        readiness.contains(&api::presence::Readiness::Initializing),
        "runner should publish Initializing before setup completes; got {readiness:?}"
    );
    assert!(
        readiness
            .iter()
            .filter(|state| **state == api::presence::Readiness::Ready)
            .count()
            >= 2,
        "idle runner should publish repeated Ready heartbeats on cadence; got {readiness:?}"
    );
    assert!(
        readiness.contains(&api::presence::Readiness::Degraded),
        "runner should publish Degraded while stopping; got {readiness:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_shutdown_hook_is_bounded_by_grace() {
    let mut launch = ParticipantLaunch::local("slow-shutdown-1", "robot");
    launch.shutdown_grace_ms = 100;
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // The hook parks for 60s; the runner must abandon it after the 100ms grace
    // and still return cleanly (closing the bus) rather than hang.
    let started = std::time::Instant::now();
    tokio::time::timeout(
        Duration::from_secs(10),
        phoxal::participant::run_with::<SlowShutdown, _, _>(launch, RealClock::new(), shutdown),
    )
    .await
    .expect("runner must not hang on a slow shutdown hook")
    .expect("runner should complete cleanly after the grace elapses");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "shutdown was not bounded by the grace (took {elapsed:?})"
    );
    assert!(
        !SLOW_SHUTDOWN_COMPLETED.load(Ordering::Relaxed),
        "the slow hook should have been abandoned at the grace, not run to completion"
    );
}

/// P6-3 acceptance: under `ClockMode::Simulation`, a `#[step]` participant's
/// ticks are released by the live `simulation/clock` feed, not a free-running
/// wall clock - the full path (runner subscribes `simulation/clock` on the
/// shared bus, drives the `SimulationScheduler` handle from it) exercised
/// exactly as a real `Simulator` kind (e.g. the Webots supervisor) would drive
/// it, minus Webots itself.
///
/// This publishes `simulation::Clock` samples the same way
/// `simulator/webots-supervisor/src/main.rs` does (owner-side publisher over
/// `api::topic::internal::new(cap).simulation().clock()`, `publish_at` an
/// explicit `LogicalTime`), and proves three things the runner's wiring must
/// get right:
/// 1. with no clock samples published yet, the participant never steps;
/// 2. each clock advance releases exactly one more step, in step with the
///    published `now_ns` (not wall time - this test's clock samples are
///    spaced far apart in real time to make that unambiguous);
/// 3. a sample with `running == false` pauses release even though logical
///    time in that same sample's envelope did advance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulation_mode_step_advances_only_with_the_clock_feed() {
    SIM_CLOCK_STEPS.store(0, Ordering::Relaxed);

    let namespace = unique_namespace("sim-clock-feed");
    let bus_config = BusConfig::in_process(namespace.clone(), "robot");
    let bus = Bus::open(bus_config).await.expect("bus should open");

    // Stand in for the Webots supervisor: the OWNER-side publisher over the
    // same `simulation().clock()` builder `simulator/webots-supervisor`
    // uses, so this test proves the runner subscribes the identical wire key
    // a real supervisor publishes (not a look-alike topic).
    let clock_publisher = Publisher::<api::simulation::Clock>::new(
        bus.clone(),
        &api::topic::internal::new(OwnerCap::__mint())
            .simulation()
            .clock(),
    )
    .expect("clock publisher should attach");

    let mut launch = ParticipantLaunch::local("sim-clock-stepper-1", "robot");
    launch.namespace = namespace;
    launch.clock = ClockMode::Simulation;

    let period_ns = 1_000_000; // 1ms period at the participant's 1000 Hz schedule.
    // A `TestClock` starts at logical `(epoch 0, time_ns 0)` (unlike `RealClock`,
    // which stamps host-wide Unix time) so the scheduler's `start` lines up with
    // the small `LogicalTime` values this test publishes below - the point being
    // tested is the scheduler tracking the *feed*, not this clock's role (which
    // only stamps `StepContext`/`produced_at_ns`, untouched by this slice).
    let runner = run_with_bus::<SimClockStepper, _, _>(&bus, launch, TestClock::new(), async {
        // No steps should have released yet: the feed has not published a
        // single sample, so the scheduler's logical time has never left
        // `start`.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            SIM_CLOCK_STEPS.load(Ordering::Relaxed),
            0,
            "no simulation/clock sample has been published yet; the step must not have released"
        );

        // Advance one period at a time, waiting for the runner to observe
        // each step before publishing the next - proves steps track the
        // feed's cadence, not a free-running timer (each iteration sleeps
        // far longer, in real time, than the participant's 1ms period).
        for step in 1..=5u64 {
            let at = LogicalTime::new(0, step * period_ns);
            clock_publisher
                .publish_at(
                    at,
                    api::simulation::Clock {
                        now_ns: step * period_ns,
                        running: true,
                    },
                )
                .await
                .expect("clock sample should publish");

            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while SIM_CLOCK_STEPS.load(Ordering::Relaxed) < step {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "step {step} did not release within 2s of its simulation/clock sample"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(
                SIM_CLOCK_STEPS.load(Ordering::Relaxed),
                step,
                "exactly one step should have released per clock advance, no more"
            );
        }

        // Pause: even though the next sample's envelope logical time DOES
        // advance, `running == false` must withhold release.
        let paused_at = LogicalTime::new(0, 6 * period_ns);
        clock_publisher
            .publish_at(
                paused_at,
                api::simulation::Clock {
                    now_ns: 6 * period_ns,
                    running: false,
                },
            )
            .await
            .expect("paused clock sample should publish");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            SIM_CLOCK_STEPS.load(Ordering::Relaxed),
            5,
            "a paused (running=false) sample must not release a step even though logical time advanced"
        );
    });

    tokio::time::timeout(Duration::from_secs(10), runner)
        .await
        .expect("runner must not hang")
        .expect("runner should complete cleanly");
    bus.close().await.expect("bus should close");

    assert_eq!(
        SIM_CLOCK_STEPS.load(Ordering::Relaxed),
        5,
        "shutdown must not have released any further steps"
    );
}

#[test]
fn new_kind_markers_are_emitted() {
    fn assert_simulator<T: phoxal::participant::IsSimulator>() {}
    fn assert_tool<T: phoxal::participant::IsTool>() {}

    assert_simulator::<WorldSimulator>();
    assert_tool::<RobotInspector>();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn driver_reads_its_bound_component_instance() {
    let launch = ParticipantLaunch::local("component-driver-1", "robot")
        .with_component_instance("tof_front");
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    phoxal::participant::run_with::<ComponentDriver, _, _>(launch, RealClock::new(), shutdown)
        .await
        .expect("driver should read its bound component instance and run cleanly");
}
