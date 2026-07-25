//! Runner integration: a scheduled participant runs steps and then shuts down
//! cleanly, a slow `#[shutdown]` hook is
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

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use phoxal::api;
use phoxal::bus::ContractBody;
use phoxal::bus::{
    CommandPublisher, DEFAULT_QUERY_TIMEOUT, Latest, OwnerCap, Querier, RobotInstant,
    StatePublisher, StepToken, TimelineAuthority, TimelineId,
};
use phoxal::participant::{ClockMode, ParticipantLaunch, TestClock};
use phoxal::prelude::*;
use phoxal::raw::{Bus, BusConfig, run_with_bus, run_with_bus_clock};

static STEPS_OBSERVED: AtomicU64 = AtomicU64::new(0);
static NAMESPACE_SEQ: AtomicU64 = AtomicU64::new(0);
static COUNTER_STEPS: AtomicU64 = AtomicU64::new(0);
static SHUTDOWN_CALLED: AtomicBool = AtomicBool::new(false);
static SLOW_SHUTDOWN_COMPLETED: AtomicBool = AtomicBool::new(false);
static RESET_FAILURE_SHUTDOWN_CALLED: AtomicBool = AtomicBool::new(false);
static SIM_CLOCK_STEPS: AtomicU64 = AtomicU64::new(0);
static SIM_CLOCK_RESETS: Mutex<Vec<(TimelineId, TimelineId)>> = Mutex::new(Vec::new());
static HOST_TOOL_TICKS: AtomicU64 = AtomicU64::new(0);
static HOST_TOOL_MESSAGES: AtomicU64 = AtomicU64::new(0);
static SIM_CLOCK_CONTEXTS: Mutex<Vec<(RobotInstant, u64)>> = Mutex::new(Vec::new());
static NO_STEP_RESETS: Mutex<Vec<(TimelineId, TimelineId)>> = Mutex::new(Vec::new());
static NO_STEP_INGRESS: AtomicU64 = AtomicU64::new(0);
static NO_STEP_RESET_ACTIVE: AtomicBool = AtomicBool::new(false);
static NO_STEP_SERVER_OVERLAP: AtomicBool = AtomicBool::new(false);

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
    target: CommandPublisher<api::drive::Target>,
    lookup: Server<api::frame::LookupRequest, api::frame::LookupResponse>,
    submap: Server<api::map::SubmapRequest, api::map::SubmapResponse>,
}

/// The committed snapshot `#[server_snapshot]` reads: the step count observed
/// as of the last commit, so a concurrent query can prove it saw REAL
/// participant state, not a stub.
struct WallFollowerSnapshot {
    steps: u64,
}

#[phoxal::service(id = "runtime-proof", config = ())]
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
                target: ctx
                    .command_publisher(api::topic::new().drive().target())
                    .await?,
                lookup: ctx.server(api::topic::new().frame().lookup()).await?,
                submap: ctx.server(api::topic::new().map().submap()).await?,
            },
        ))
    }

    #[step(hz = 200)]
    async fn step(&mut self, api: &mut Self::Api, _step: StepContext) -> Result<()> {
        self.steps += 1;
        STEPS_OBSERVED.store(self.steps, Ordering::Relaxed);
        api.target.send(api::drive::Target {
            linear_x_mps: 0.5,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        })?;
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
        api.target.send(api::drive::Target {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        })?;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_model_participant_runs_through_a_real_bus() {
    let bus = Bus::open(BusConfig::in_process(unique_namespace("runner"), "robot"))
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
    let runtime_latest = Latest::<api::tool::runtime::Rollup>::new(
        &bus,
        &api::topic::new().tool().runtime().rollup(),
    )
    .await
    .expect("subscribe runner performance");
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

    let launch = ParticipantLaunch::local("wall-follower-1", "robot");
    let runner = run_with_bus::<WallFollower, _>(&bus, launch, async {
        tokio::time::sleep(Duration::from_millis(1_200)).await
    });

    let queries = async {
        // Give the 200 Hz step loop a head start so a committed snapshot and
        // at least one publish exist before querying.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let lookup_reply = lookup_querier
            .query(api::frame::LookupRequest {
                target_frame_id: "map".to_string(),
                source_frame_id: "base".to_string(),
                at: None,
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

    let runtime = runtime_latest
        .latest()
        .expect("the runner should publish a portable rollup without participant-authored code");
    let step = runtime
        .step
        .expect("a scheduled participant should report step timing");
    assert!(step.completed > 0);
    assert_eq!(step.target_period_ns, 5_000_000);
    assert!(runtime.topics.iter().any(|row| {
        row.topic == "v0.1/drive/target"
            && row.direction == api::tool::RuntimeDirection::Publish
            && row.buffer_kind == api::tool::RuntimeBufferKind::Outbound
    }));
}

// ---------------------------------------------------------------------------
// Subscriber + Latest under the owned/Arc-shared split (F-runtime review gap)
// ---------------------------------------------------------------------------

// The first test omits the two `Api` field kinds whose sharing semantics the
// reviewer flagged: `Subscriber` (a DESTRUCTIVE shared queue - two clones
// compete) and `Latest` as an INBOUND handle (a non-destructive read from one
// mutex-serialized retained slot). This second test drives a participant that
// owns BOTH through the same owned-`api` / `Arc<Self::Api>`-snapshot split the
// runner applies, and proves the SAFE path: the `#[step]` loop (which holds
// `&mut Self::Api`) drains ALL samples on its `Subscriber` and reads its
// `Latest` correctly, WHILE a concurrent `#[server_snapshot]` runs against the
// shared `Arc<Self::Api>` clone - a snapshot server that reads committed
// `Snapshot` state and never `recv`s the `Subscriber`, so it steals nothing.
// This is the field-kind coverage the D3 proof above is missing.
//
// It also proves that two distinct contracts from the train-selected API can
// share one participant and round-trip real bytes on one live in-process bus.

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
    // Client-side keep-last-1 of the `v0.1/battery/state` STATE (owner
    // publishes, client subscribes), alongside the command field above. This
    // is the non-destructive inbound field.
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
#[phoxal::service(id = "drain-proof", config = (), api = DrainApi)]
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
        unique_namespace("drain-proof"),
        "robot",
    ))
    .await
    .expect("open shared bus");

    // Companion client handles on the same bus. `drive/target` is a command
    // (client publishes), while `battery/state` is state (owner publishes), so
    // the companion takes the
    // client side of the former and the owner side of the latter - the mirror
    // of what `Drainer` subscribes. Two contracts, one bus, one participant.
    let target_pub = CommandPublisher::<api::drive::Target>::new(
        bus.clone(),
        &api::topic::new().drive().target(),
    )
    .expect("build target publisher");
    let battery_topic = api::topic::internal::new(OwnerCap::__mint())
        .battery()
        .state();
    assert_eq!(
        <api::battery::State as ContractBody>::TOPIC,
        "v0.1/battery/state",
        "the moved contract's version-qualified wire key (D1)"
    );
    assert_eq!(battery_topic.key(), "v0.1/battery/state");
    let battery_pub = StatePublisher::<api::battery::State>::new(bus.clone(), &battery_topic)
        .expect("build battery publisher");
    let query_querier = Querier::<api::map::SubmapRequest, api::map::SubmapResponse>::new(
        bus.clone(),
        &api::topic::new().map().submap(),
        DEFAULT_QUERY_TIMEOUT,
    )
    .expect("build submap querier");

    let launch = ParticipantLaunch::local("drain-proof-1", "robot");
    let runner = run_with_bus::<Drainer, _>(&bus, launch, async {
        tokio::time::sleep(Duration::from_millis(800)).await
    });

    let driver = async {
        // Let `#[setup]` build the subscription + latest before publishing, so
        // no sample is emitted before the drainer is listening.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // One battery state for the `Latest` field.
        let line = TimelineId::mint();
        battery_pub
            .publish(
                &StepToken::__mint(RobotInstant::new(line, 1)),
                api::battery::State {
                    voltage_v: DRAIN_VOLTAGE_V,
                    current_a: 1.0,
                    charge_ratio: 0.9,
                },
            )
            .expect("publish battery state");

        // Feed exactly `DRAIN_COMMANDS` commands, spaced so the 200 Hz step
        // loop drains between them (well under the 32-deep ring), then query
        // the snapshot server CONCURRENTLY - it must answer from committed
        // state while the step loop keeps draining, stealing nothing.
        for _ in 0..DRAIN_COMMANDS {
            target_pub
                .send(api::drive::Target {
                    linear_x_mps: 1.0,
                    angular_z_radps: 0.0,
                    curvature_limit_radpm: None,
                })
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

    // The `Latest` field read the current value through the same owned `api` -
    // the ground-breaker proof itself: a real `v0.1/battery/state` publish
    // was delivered over the live bus and observed correctly, WHILE the
    // sibling `v0.1/drive/target` command round-tripped on the same
    // participant/bus at the same time (asserted above).
    assert_eq!(
        f32::from_bits(DRAIN_LAST_VOLTAGE_BITS.load(Ordering::Relaxed) as u32),
        DRAIN_VOLTAGE_V,
        "the step loop should read the published battery voltage through its Latest field"
    );
}

// ---------------------------------------------------------------------------
// Runner-level behavior: shutdown/grace/simulation-clock proofs
// ---------------------------------------------------------------------------

#[derive(phoxal::Api)]
struct CounterApi {
    target: CommandPublisher<api::drive::Target>,
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
                target: ctx
                    .command_publisher(api::topic::new().drive().target())
                    .await?,
            },
        ))
    }

    #[step(hz = 200)]
    async fn step(&mut self, api: &mut Self::Api, _step: StepContext) -> Result<()> {
        COUNTER_STEPS.fetch_add(1, Ordering::Relaxed);
        api.target.send(api::drive::Target {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        })?;
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _api: &mut Self::Api) -> Result<()> {
        SHUTDOWN_CALLED.store(true, Ordering::Relaxed);
        Ok(())
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

#[phoxal::service(id = "reset-failure", config = (), api = ())]
struct ResetFailure;

#[phoxal::behavior]
impl ResetFailure {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[reset]
    async fn reset(&mut self, _ctx: ResetContext) -> Result<()> {
        Err(anyhow::anyhow!("intentional reset failure"))
    }

    #[shutdown]
    async fn shutdown(&mut self, _api: &mut Self::Api) -> Result<()> {
        RESET_FAILURE_SHUTDOWN_CALLED.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// Acceptance fixture (P6-3): a `#[step]` participant with no other IO, used
/// to prove `#[step]` ticks under `ClockMode::Simulation` release only as the
/// live `simulation/clock` feed advances (see `simulation_mode_step_advances_only_with_the_clock_feed`).
#[derive(phoxal::Api)]
struct SimClockApi {
    target: CommandPublisher<api::drive::Target>,
}

#[phoxal::service(id = "sim-clock-stepper", config = (), api = SimClockApi)]
struct SimClockStepper;

#[phoxal::behavior]
impl SimClockStepper {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                target: ctx
                    .command_publisher(api::topic::new().drive().target())
                    .await?,
            },
        ))
    }

    #[reset]
    async fn reset(&mut self, ctx: ResetContext) -> Result<()> {
        SIM_CLOCK_RESETS
            .lock()
            .expect("simulation reset log poisoned")
            .push((ctx.previous_timeline(), ctx.new_timeline()));
        Ok(())
    }

    #[step(hz = 1000)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        SIM_CLOCK_CONTEXTS
            .lock()
            .expect("simulation context log poisoned")
            .push((step.now(), step.step_index()));
        SIM_CLOCK_STEPS.fetch_add(1, Ordering::Relaxed);
        api.target.send(api::drive::Target {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        })?;
        Ok(())
    }
}

#[derive(phoxal::Api)]
struct NoStepResetApi {
    battery: Subscriber<api::battery::State>,
    lookup: Server<api::frame::LookupRequest, api::frame::LookupResponse>,
}

#[phoxal::service(id = "no-step-reset-observer", config = (), api = NoStepResetApi)]
struct NoStepResetObserver;

#[phoxal::behavior]
impl NoStepResetObserver {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let battery = ctx
            .subscriber(api::topic::new().battery().state(), 8)
            .await?;
        let receiver = battery.clone();
        ctx.spawn_managed("no-step-ingress", async move {
            while receiver.recv().await.is_ok() {
                NO_STEP_INGRESS.fetch_add(1, Ordering::Relaxed);
            }
        });
        // Keep setup in flight long enough for the test's first controller
        // clock to become current before main_loop clones the watch receiver.
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok((
            Self,
            Self::Api {
                battery,
                lookup: ctx.server(api::topic::new().frame().lookup()).await?,
            },
        ))
    }

    #[reset]
    async fn reset(&mut self, ctx: ResetContext) -> Result<()> {
        NO_STEP_RESET_ACTIVE.store(true, Ordering::SeqCst);
        NO_STEP_RESETS
            .lock()
            .expect("no-step reset log poisoned")
            .push((ctx.previous_timeline(), ctx.new_timeline()));
        tokio::time::sleep(Duration::from_millis(80)).await;
        NO_STEP_RESET_ACTIVE.store(false, Ordering::SeqCst);
        Ok(())
    }

    #[server(api = lookup)]
    async fn lookup(
        &mut self,
        _api: &mut Self::Api,
        _request: api::frame::LookupRequest,
    ) -> ServerResult<api::frame::LookupResponse> {
        if NO_STEP_RESET_ACTIVE.load(Ordering::SeqCst) {
            NO_STEP_SERVER_OVERLAP.store(true, Ordering::SeqCst);
        }
        Ok(api::frame::LookupResponse { transform: None })
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
        let _ = step.now();
        Ok(())
    }
}

#[phoxal::tool(id = "robot-inspector")]
struct RobotInspector;

#[phoxal::behavior]
impl RobotInspector {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let _ = ctx.robot();
        Ok((Self, ()))
    }
}

#[phoxal::tool(id = "host-driven-tool")]
struct HostDrivenTool;

#[phoxal::behavior]
impl HostDrivenTool {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let bus = ctx.raw_bus();
        let manual = phoxal::raw::Subscriber::<api::motion::ManualCommand>::new(
            &bus,
            &api::topic::internal::new(ctx.owner_capability())
                .motion()
                .manual(),
            8,
        )
        .await?;
        ctx.spawn_managed("host-ticker", async {
            let mut interval = tokio::time::interval(Duration::from_millis(10));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                HOST_TOOL_TICKS.fetch_add(1, Ordering::Relaxed);
            }
        });
        ctx.spawn_managed("raw-subscriber", async move {
            while manual.recv().await.is_ok() {
                HOST_TOOL_MESSAGES.fetch_add(1, Ordering::Relaxed);
            }
        });
        Ok((Self, ()))
    }
}

#[derive(serde::Deserialize, phoxal::Config)]
struct ConfiguredInspectorConfig {
    label: String,
}

#[phoxal::tool(id = "configured-inspector", config = ConfiguredInspectorConfig)]
struct ConfiguredInspector;

#[phoxal::behavior]
impl ConfiguredInspector {
    #[setup]
    async fn setup(
        _ctx: &mut SetupContext<Self>,
        config: Self::Config,
    ) -> Result<(Self, Self::Api)> {
        let _ = config.label;
        Ok((Self, ()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configless_tool_accepts_absent_config_but_configured_tool_rejects_it() {
    let configless = ParticipantLaunch::local("robot-inspector", "robot");
    phoxal::participant::run_with::<RobotInspector, _>(configless, async {})
        .await
        .expect("a tool with omitted config type should accept absent PHOXAL_CONFIG");

    let configured = ParticipantLaunch::local("configured-inspector", "robot");
    let error = phoxal::participant::run_with::<ConfiguredInspector, _>(configured, async {})
        .await
        .expect_err("a tool with an explicit non-optional config should require PHOXAL_CONFIG");
    assert!(
        error.to_string().contains("invalid type: null"),
        "unexpected absent-config error: {error:#}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clockless_tool_keeps_host_work_and_raw_subscriptions_running() {
    HOST_TOOL_TICKS.store(0, Ordering::Relaxed);
    HOST_TOOL_MESSAGES.store(0, Ordering::Relaxed);
    let participant_id = "host-driven-tool-1";
    let namespace = unique_namespace("host-driven-tool");
    let mut bus_config = BusConfig::in_process(namespace.clone(), "robot");
    bus_config.participant = participant_id.to_string();
    let bus = Bus::open(bus_config).await.expect("bus should open");
    let manual =
        phoxal::raw::CommandPublisher::new(bus.clone(), &api::topic::new().motion().manual())
            .expect("manual publisher should attach");

    let mut launch = ParticipantLaunch::local(participant_id, "robot");
    launch.namespace = namespace;
    run_with_bus::<HostDrivenTool, _>(&bus, launch, async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        manual
            .send(api::motion::ManualCommand {
                linear_x_mps: 0.2,
                angular_z_radps: 0.0,
            })
            .expect("raw tool input should publish");
        tokio::time::sleep(Duration::from_millis(80)).await;
    })
    .await
    .expect("tool should run without a logical clock input");

    assert!(
        HOST_TOOL_TICKS.load(Ordering::Relaxed) >= 2,
        "host ticker must run without a logical clock"
    );
    assert_eq!(
        HOST_TOOL_MESSAGES.load(Ordering::Relaxed),
        1,
        "raw tool subscriptions must run without a logical clock"
    );
    bus.close().await.expect("bus should close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runner_runs_steps_then_shuts_down_cleanly() {
    let launch = ParticipantLaunch::local("counter-1", "robot");
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    phoxal::participant::run_with::<Counter, _>(launch, shutdown)
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
        phoxal::participant::run_with::<SlowShutdown, _>(launch, shutdown),
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
/// exactly as a real `Simulator` kind (the Webots controller) would drive
/// it, minus Webots itself.
///
/// This publishes `simulation::Clock` samples the same way
/// `simulator/webots-controller/src/lib.rs` does (owner-side publisher over
/// `api::topic::internal::new(cap).simulation().clock()`, `publish_at` an
/// explicit `RobotInstant`), and proves three things the runner's wiring must
/// get right:
/// 1. with no clock samples published yet, the participant never steps;
/// 2. each clock advance releases exactly one more step, in step with the
///    published `now_ns` (not wall time - this test's clock samples are
///    spaced far apart in real time to make that unambiguous);
/// 3. publishing no new sample leaves the participant stopped at its last
///    logical step.
// Stands in for one controller process, which owns at most one timeline
// authority; these must not overlap inside the shared test binary.
#[serial_test::serial(timeline_authority)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulation_mode_step_advances_only_with_the_clock_feed() {
    SIM_CLOCK_STEPS.store(0, Ordering::Relaxed);
    SIM_CLOCK_RESETS
        .lock()
        .expect("simulation reset log poisoned")
        .clear();
    SIM_CLOCK_CONTEXTS
        .lock()
        .expect("simulation context log poisoned")
        .clear();

    let namespace = unique_namespace("sim-clock-feed");
    let bus_config = BusConfig::in_process(namespace.clone(), "robot");
    let bus = Bus::open(bus_config).await.expect("bus should open");

    // Stand in for the Webots controller: the OWNER-side publisher over the
    // same `simulation().clock()` builder it uses, so this test proves the
    // runner subscribes the identical wire key (not a look-alike topic).
    let clock_publisher = StatePublisher::<api::simulation::Clock>::new(
        bus.clone(),
        &api::topic::internal::new(OwnerCap::__mint())
            .simulation()
            .clock(),
    )
    .expect("clock publisher should attach");
    let first_timeline = TimelineId::from_raw(9).expect("timeline must be nonzero");
    let replacement = TimelineId::from_raw(1).expect("timeline must be nonzero");
    let higher_replacement = TimelineId::from_raw(12).expect("timeline must be nonzero");
    let mut authority =
        TimelineAuthority::__mint(first_timeline).expect("world authority should mint");
    let target_subscriber = Subscriber::<api::drive::Target>::new(
        &bus,
        &api::topic::internal::new(OwnerCap::__mint())
            .drive()
            .target(),
        16,
    )
    .await
    .expect("target subscriber should attach");

    let mut launch = ParticipantLaunch::local("sim-clock-stepper-1", "robot");
    launch.namespace = namespace;
    launch.clock = ClockMode::Simulation;

    let period_ns = 1_000_000; // 1ms period at the participant's 1000 Hz schedule.
    // Deliberately move the injected clock far away from the feed. Simulation
    // mode must ignore it for StepContext and publication timestamps.
    let injected_clock = TestClock::new();
    injected_clock.advance(Duration::from_secs(123));
    let runner = run_with_bus_clock::<SimClockStepper, _, _>(&bus, launch, injected_clock, async {
        // No steps should have released yet: the feed has not published a
        // single sample, so the scheduler's logical time has never left
        // `start`.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            SIM_CLOCK_STEPS.load(Ordering::Relaxed),
            0,
            "no simulation/clock sample has been published yet; the step must not have released"
        );
        clock_publisher
            .publish(
                &authority.completed_step(0),
                api::simulation::Clock { step: 0 },
            )
            .expect("initial timeline clock should publish");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            SIM_CLOCK_RESETS
                .lock()
                .expect("simulation reset log poisoned")
                .as_slice(),
            &[],
            "the first observed timeline must not invoke reset"
        );

        // Advance one period at a time, waiting for the runner to observe
        // each step before publishing the next - proves steps track the
        // feed's cadence, not a free-running timer (each iteration sleeps
        // far longer, in real time, than the participant's 1ms period).
        for step in 1..=5u64 {
            let at = RobotInstant::new(first_timeline, step * period_ns);
            clock_publisher
                .publish(
                    &authority.completed_step(step * period_ns),
                    api::simulation::Clock { step },
                )
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
            let published = tokio::time::timeout(Duration::from_secs(2), target_subscriber.recv())
                .await
                .expect("step publication should arrive")
                .expect("step publication should decode");
            assert_eq!(
                published.metadata.produced_at, None,
                "a command expresses no robot time, whatever the step it was sent from"
            );
            let _ = at;
        }

        // No publication means no world advance and therefore no service
        // step.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            SIM_CLOCK_STEPS.load(Ordering::Relaxed),
            5,
            "the service must not advance without a new simulation/clock sample"
        );

        // Reset to a new epoch at time zero. The old epoch's pending target
        // must be discarded without releasing a step or spinning the loop.
        authority.replace_timeline(replacement);
        clock_publisher
            .publish(
                &authority.completed_step(0),
                api::simulation::Clock { step: 0 },
            )
            .expect("reset clock sample should publish");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            SIM_CLOCK_STEPS.load(Ordering::Relaxed),
            5,
            "a timeline replacement must rebase the target without releasing or spinning"
        );

        let first_after_reset = RobotInstant::new(replacement, period_ns);
        clock_publisher
            .publish(
                &authority.completed_step(period_ns),
                api::simulation::Clock { step: 1 },
            )
            .expect("first post-reset clock sample should publish");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while SIM_CLOCK_STEPS.load(Ordering::Relaxed) < 6 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "first post-reset step did not release"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        {
            let contexts = SIM_CLOCK_CONTEXTS
                .lock()
                .expect("simulation context log poisoned");
            assert_eq!(contexts.last(), Some(&(first_after_reset, 0)));
        }
        assert_eq!(
            SIM_CLOCK_RESETS
                .lock()
                .expect("simulation reset log poisoned")
                .as_slice(),
            &[(first_timeline, replacement)],
            "a replacement timeline must reset exactly once before its first step"
        );

        // A late clock from the retired controller must not reactivate its
        // execution. In particular it cannot run a second reset or release a
        // step stamped in epoch 9 after epoch 1 has become active.
        authority.replace_timeline(first_timeline);
        clock_publisher
            .publish(
                &authority.completed_step(6 * period_ns),
                api::simulation::Clock { step: 6 },
            )
            .expect("late retired clock sample should publish at the bus layer");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            SIM_CLOCK_STEPS.load(Ordering::Relaxed),
            6,
            "a late retired clock must not release a step"
        );
        assert_eq!(
            SIM_CLOCK_RESETS
                .lock()
                .expect("simulation reset log poisoned")
                .as_slice(),
            &[(first_timeline, replacement)],
            "first -> replacement -> late first must perform exactly the original reset"
        );
        assert_eq!(
            SIM_CLOCK_CONTEXTS
                .lock()
                .expect("simulation context log poisoned")
                .last(),
            Some(&(first_after_reset, 0)),
            "no retired-timeline StepContext may appear after the replacement reset"
        );

        // Epochs are opaque identities, so a numerically higher replacement
        // must follow the same reset path as the lower-valued replacement.
        authority.replace_timeline(higher_replacement);
        clock_publisher
            .publish(
                &authority.completed_step(0),
                api::simulation::Clock { step: 0 },
            )
            .expect("higher-valued reset clock sample should publish");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            SIM_CLOCK_STEPS.load(Ordering::Relaxed),
            6,
            "a higher-valued replacement must rebase without releasing a step"
        );

        let first_after_higher_reset = RobotInstant::new(higher_replacement, period_ns);
        clock_publisher
            .publish(
                &authority.completed_step(period_ns),
                api::simulation::Clock { step: 1 },
            )
            .expect("first step after higher-valued reset should publish");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while SIM_CLOCK_STEPS.load(Ordering::Relaxed) < 7 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "first step after higher-valued reset did not release"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        {
            let contexts = SIM_CLOCK_CONTEXTS
                .lock()
                .expect("simulation context log poisoned");
            assert_eq!(contexts.last(), Some(&(first_after_higher_reset, 0)));
        }
        assert_eq!(
            SIM_CLOCK_RESETS
                .lock()
                .expect("simulation reset log poisoned")
                .as_slice(),
            &[
                (first_timeline, replacement),
                (replacement, higher_replacement)
            ],
            "every differing timeline must reset exactly once before its first step"
        );
    });

    tokio::time::timeout(Duration::from_secs(10), runner)
        .await
        .expect("runner must not hang")
        .expect("runner should complete cleanly");
    bus.close().await.expect("bus should close");

    assert_eq!(
        SIM_CLOCK_STEPS.load(Ordering::Relaxed),
        7,
        "shutdown must not have released any further steps"
    );
}

// Stands in for one controller process, which owns at most one timeline
// authority; these must not overlap inside the shared test binary.
#[serial_test::serial(timeline_authority)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_step_service_observes_timeline_changes_and_installs_startup_barrier() {
    NO_STEP_INGRESS.store(0, Ordering::Relaxed);
    NO_STEP_RESET_ACTIVE.store(false, Ordering::Relaxed);
    NO_STEP_SERVER_OVERLAP.store(false, Ordering::Relaxed);
    NO_STEP_RESETS
        .lock()
        .expect("no-step reset log poisoned")
        .clear();

    let namespace = unique_namespace("no-step-reset");
    let bus = Bus::open(BusConfig::in_process(namespace.clone(), "robot"))
        .await
        .expect("bus should open");
    let clock = StatePublisher::<api::simulation::Clock>::new(
        bus.clone(),
        &api::topic::internal::new(OwnerCap::__mint())
            .simulation()
            .clock(),
    )
    .expect("clock publisher should attach");
    let battery = StatePublisher::<api::battery::State>::new(
        bus.clone(),
        &api::topic::internal::new(OwnerCap::__mint())
            .battery()
            .state(),
    )
    .expect("battery publisher should attach");
    let lookup = Querier::<api::frame::LookupRequest, api::frame::LookupResponse>::new(
        bus.clone(),
        &api::topic::new().frame().lookup(),
        DEFAULT_QUERY_TIMEOUT,
    )
    .expect("lookup querier should attach");

    let first = TimelineId::from_raw(7).expect("timeline must be nonzero");
    let retired = TimelineId::from_raw(6).expect("timeline must be nonzero");
    let replacement = TimelineId::from_raw(3).expect("timeline must be nonzero");
    let traffic = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let mut authority = TimelineAuthority::__mint(first).expect("world authority should mint");
        clock
            .publish(
                &authority.completed_step(10),
                api::simulation::Clock { step: 1 },
            )
            .expect("initial clock should publish during setup");

        tokio::time::sleep(Duration::from_millis(180)).await;
        // One state sample from a retired world and one from the current one:
        // only the current one may become visible.
        for (timeline, voltage) in [(retired, 6.0), (first, 7.0)] {
            battery
                .publish(
                    &StepToken::__mint(RobotInstant::new(timeline, 20)),
                    api::battery::State {
                        voltage_v: voltage,
                        current_a: 0.0,
                        charge_ratio: 1.0,
                    },
                )
                .expect("battery state should publish");
        }

        tokio::time::sleep(Duration::from_millis(80)).await;
        authority.replace_timeline(replacement);
        clock
            .publish(
                &authority.completed_step(0),
                api::simulation::Clock { step: 0 },
            )
            .expect("replacement clock should publish");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while !NO_STEP_RESET_ACTIVE.load(Ordering::SeqCst) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "reset did not begin"
            );
            tokio::task::yield_now().await;
        }
        lookup
            .query(api::frame::LookupRequest {
                target_frame_id: "map".to_string(),
                source_frame_id: "base".to_string(),
                at: None,
            })
            .await
            .expect("exclusive query should run after reset completes");
    });

    let mut launch = ParticipantLaunch::local("no-step-reset-observer-1", "robot");
    launch.namespace = namespace;
    launch.clock = ClockMode::Simulation;
    run_with_bus::<NoStepResetObserver, _>(&bus, launch, async {
        tokio::time::sleep(Duration::from_millis(500)).await;
    })
    .await
    .expect("step-less service should run cleanly");
    traffic.await.expect("traffic task should complete");

    assert_eq!(
        NO_STEP_INGRESS.load(Ordering::Relaxed),
        1,
        "startup barrier must reject the retired-timeline sample and preserve the current one"
    );
    assert_eq!(
        NO_STEP_RESETS
            .lock()
            .expect("no-step reset log poisoned")
            .as_slice(),
        &[(first, replacement)],
        "a step-less service must reset exactly once on a replacement timeline"
    );
    assert!(
        !NO_STEP_SERVER_OVERLAP.load(Ordering::SeqCst),
        "reset and exclusive server handling must be serialized"
    );
    bus.close().await.expect("bus should close");
}

// Stands in for one controller process, which owns at most one timeline
// authority; these must not overlap inside the shared test binary.
#[serial_test::serial(timeline_authority)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_error_faults_the_runner_and_still_runs_teardown() {
    RESET_FAILURE_SHUTDOWN_CALLED.store(false, Ordering::Relaxed);
    let namespace = unique_namespace("reset-failure");
    let bus = Bus::open(BusConfig::in_process(namespace.clone(), "robot"))
        .await
        .expect("bus should open");
    let clock = StatePublisher::<api::simulation::Clock>::new(
        bus.clone(),
        &api::topic::internal::new(OwnerCap::__mint())
            .simulation()
            .clock(),
    )
    .expect("clock publisher should attach");
    let traffic = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let mut authority =
            TimelineAuthority::__mint(TimelineId::from_raw(4).expect("timeline must be nonzero"))
                .expect("world authority should mint");
        for (timeline, delay_ms) in [(4, 100), (8, 0)] {
            authority.replace_timeline(TimelineId::from_raw(timeline).expect("nonzero timeline"));
            clock
                .publish(
                    &authority.completed_step(0),
                    api::simulation::Clock { step: 0 },
                )
                .expect("clock should publish");
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    });

    let mut launch = ParticipantLaunch::local("reset-failure-1", "robot");
    launch.namespace = namespace;
    launch.clock = ClockMode::Simulation;
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        run_with_bus::<ResetFailure, _>(&bus, launch, std::future::pending()),
    )
    .await
    .expect("reset failure should terminate the runner")
    .expect_err("reset failure must fault the runner");
    assert!(
        error.to_string().contains("intentional reset failure"),
        "unexpected reset error: {error:#}"
    );
    assert!(
        RESET_FAILURE_SHUTDOWN_CALLED.load(Ordering::Relaxed),
        "reset failure must still execute normal teardown"
    );
    traffic.await.expect("traffic task should complete");
    bus.close().await.expect("bus should close");
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

    phoxal::participant::run_with::<ComponentDriver, _>(launch, shutdown)
        .await
        .expect("driver should read its bound component instance and run cleanly");
}
