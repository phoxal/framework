//! Runner integration: a scheduled participant runs steps and then shuts down
//! cleanly, and its `emit-apis` document matches the frozen schema (D50).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use phoxal::bus::{ContractBody, LogicalTime, OwnerCap, Publisher, Subscriber};
use phoxal::participant::{
    ClockMode, ParticipantLaunch, RealClock, TestClock, emit_apis_json, run_with,
};
use phoxal::prelude::*;
use phoxal::raw::{Bus, BusConfig, run_with_bus};
use phoxal_api::y2026_1 as api;

static STEPS: AtomicU64 = AtomicU64::new(0);
static NAMESPACE_SEQ: AtomicU64 = AtomicU64::new(0);
static SHUTDOWN_CALLED: AtomicBool = AtomicBool::new(false);
static SLOW_SHUTDOWN_COMPLETED: AtomicBool = AtomicBool::new(false);
static SIM_CLOCK_STEPS: AtomicU64 = AtomicU64::new(0);

#[derive(phoxal::Service)]
#[phoxal(id = "counter", api = y2026_1)]
struct Counter {
    target: Publisher<api::drive::Target>,
}

#[derive(phoxal::Service)]
#[phoxal(id = "idle-presence", api = y2026_1)]
struct IdlePresence {}

#[derive(serde::Deserialize, phoxal::schemars::JsonSchema)]
struct CounterConfig {
    gain: f64,
    enabled: bool,
}

#[derive(phoxal::Service)]
#[phoxal(id = "configured-counter", api = y2026_1, config = CounterConfig)]
struct ConfiguredCounter {
    _gain: f64,
}

#[allow(dead_code)]
#[derive(phoxal::Service)]
#[phoxal(
    id = "explicit-contracts",
    api = y2026_1,
    contracts(
        publishes(api::drive::Target),
        subscribes(api::map::Revision),
        queries(api::map::SubmapRequest => api::map::SubmapResponse),
    )
)]
struct ExplicitContracts {
    // Deliberately duplicates the explicit publishes(...) entry above.
    target: Publisher<api::drive::Target>,
}

#[phoxal::behavior]
impl Counter {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            target: ctx.publisher(api::topic::new().drive().target()).await?,
        })
    }

    #[step(hz = 200)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        STEPS.fetch_add(1, Ordering::Relaxed);
        self.target
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
    async fn shutdown(&mut self) -> Result<()> {
        SHUTDOWN_CALLED.store(true, Ordering::Relaxed);
        Ok(())
    }
}

#[phoxal::behavior]
impl IdlePresence {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }
}

#[derive(phoxal::Service)]
#[phoxal(id = "slow-shutdown", api = y2026_1)]
struct SlowShutdown {}

/// Acceptance fixture (P6-3): a `#[step]` participant with no other IO, used
/// to prove `#[step]` ticks under `ClockMode::Simulation` release only as the
/// live `simulation/clock` feed advances (see `simulation_mode_step_advances_only_with_the_clock_feed`).
#[derive(phoxal::Service)]
#[phoxal(id = "sim-clock-stepper", api = y2026_1)]
struct SimClockStepper {}

#[derive(phoxal::Driver)]
#[phoxal(id = "component-driver", api = y2026_1)]
struct ComponentDriver {}

#[derive(phoxal::Simulator)]
#[phoxal(id = "world-simulator", api = y2026_1)]
struct WorldSimulator {}

#[derive(phoxal::Tool)]
#[phoxal(id = "robot-inspector", api = y2026_1)]
struct RobotInspector {}

#[phoxal::behavior]
impl SlowShutdown {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[shutdown]
    async fn shutdown(&mut self, ctx: ShutdownContext) -> Result<()> {
        // Park far longer than the runner's grace. If the grace were not
        // enforced, `run_with` would block here for 60s.
        let _ = ctx.grace();
        tokio::time::sleep(Duration::from_secs(60)).await;
        SLOW_SHUTDOWN_COMPLETED.store(true, Ordering::Relaxed);
        Ok(())
    }
}

#[phoxal::behavior]
impl SimClockStepper {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[step(hz = 1000)]
    async fn step(&mut self, _step: StepContext) -> Result<()> {
        SIM_CLOCK_STEPS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[phoxal::behavior]
impl ComponentDriver {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let _component = ctx.component()?;
        Ok(Self {})
    }
}

#[phoxal::behavior]
impl WorldSimulator {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[step(hz = 20)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let _ = step.time();
        Ok(())
    }
}

#[phoxal::behavior]
impl RobotInspector {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let _ = ctx.robot();
        Ok(Self {})
    }
}

#[phoxal::behavior]
impl ConfiguredCounter {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>, config: Self::Config) -> Result<Self> {
        let gain = if config.enabled { config.gain } else { 0.0 };
        Ok(Self { _gain: gain })
    }
}

#[phoxal::behavior]
impl ExplicitContracts {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            target: ctx.publisher(api::topic::new().drive().target()).await?,
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runner_runs_steps_then_shuts_down_cleanly() {
    let launch = ParticipantLaunch::local("counter-1", "robot");
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    run_with::<Counter, _, _>(launch, RealClock::new(), shutdown)
        .await
        .expect("runner should complete cleanly");

    assert!(
        STEPS.load(Ordering::Relaxed) > 0,
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
    let heartbeats = Subscriber::<api::presence::Heartbeat>::new(&bus, &heartbeat_topic, 16)
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
        run_with::<SlowShutdown, _, _>(launch, RealClock::new(), shutdown),
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
fn emit_apis_reports_frozen_schema() {
    let json = emit_apis_json::<Counter>();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(value["schema"], "phoxal.emit-apis/v0");
    assert_eq!(value["artifact"]["kind"], "service");
    assert_eq!(value["artifact"]["id"], "counter");
    assert_eq!(value["api_version"], "y2026_1");
    assert_eq!(value["participant_class"], "checked");
    assert_eq!(value["bus_abi"], "phoxal-bus/v0");

    let contracts = value["required_contracts"].as_array().unwrap();
    assert!(
        contracts.iter().any(|c| {
            c["api_version"] == "y2026_1"
                && c["schema_id"].as_str().is_some_and(|id| id.len() == 16)
                && c["family"] == <api::drive::Target as ContractBody>::FAMILY
        }),
        "emit-apis should report the drive::Target contract"
    );
}

#[test]
fn emit_apis_reports_driver_kind() {
    let json = emit_apis_json::<ComponentDriver>();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(value["artifact"]["kind"], "driver");
    assert_eq!(value["artifact"]["id"], "component-driver");
}

#[test]
fn emit_apis_reports_simulator_kind() {
    let json = emit_apis_json::<WorldSimulator>();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(value["artifact"]["kind"], "simulator");
    assert_eq!(value["artifact"]["id"], "world-simulator");
}

#[test]
fn emit_apis_reports_tool_kind() {
    let json = emit_apis_json::<RobotInspector>();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(value["artifact"]["kind"], "tool");
    assert_eq!(value["artifact"]["id"], "robot-inspector");
    assert_eq!(value["participant_class"], "privileged");
    assert_eq!(value["required_contracts"].as_array().unwrap().len(), 0);
}

#[test]
fn new_kind_markers_are_emitted() {
    fn assert_simulator<T: phoxal::participant::IsSimulator>() {}
    fn assert_tool<T: phoxal::participant::IsTool>() {}

    assert_simulator::<WorldSimulator>();
    assert_tool::<RobotInspector>();
}

#[test]
fn emit_apis_reports_config_schema() {
    let json = emit_apis_json::<ConfiguredCounter>();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(value["artifact"]["id"], "configured-counter");
    let schema = &value["config_schema"];
    assert!(
        schema.to_string().contains("gain") && schema.to_string().contains("enabled"),
        "config schema should describe CounterConfig, got {schema}"
    );
}

#[test]
fn emit_apis_reports_explicit_contracts_once() {
    let json = emit_apis_json::<ExplicitContracts>();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let contracts = value["required_contracts"].as_array().unwrap();

    let publish_drive_target = contracts
        .iter()
        .filter(|c| {
            c["api_version"] == "y2026_1"
                && c["family"] == <api::drive::Target as ContractBody>::FAMILY
        })
        .count();
    assert_eq!(publish_drive_target, 1);
    assert!(contracts.iter().any(|c| {
        c["api_version"] == "y2026_1" && c["family"] == <api::map::Revision as ContractBody>::FAMILY
    }));
    assert!(contracts.iter().any(|c| {
        c["api_version"] == "y2026_1"
            && c["family"] == <api::map::SubmapRequest as ContractBody>::FAMILY
    }));
    assert!(contracts.iter().any(|c| {
        c["api_version"] == "y2026_1"
            && c["family"] == <api::map::SubmapResponse as ContractBody>::FAMILY
    }));
}

fn unique_namespace(label: &str) -> String {
    let seq = NAMESPACE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("test/{label}/{}/{}", std::process::id(), seq)
}
