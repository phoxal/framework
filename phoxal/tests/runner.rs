//! Runner integration: a scheduled participant runs steps and then shuts down
//! cleanly, and its `emit-apis` document matches the frozen schema (D50).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use phoxal::participant::{ParticipantLaunch, RealClock, emit_apis_json, run_with};
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

static STEPS: AtomicU64 = AtomicU64::new(0);
static SHUTDOWN_CALLED: AtomicBool = AtomicBool::new(false);
static SLOW_SHUTDOWN_COMPLETED: AtomicBool = AtomicBool::new(false);

#[derive(phoxal::Service)]
#[phoxal(id = "counter", api = y2026_1)]
struct Counter {
    target: Publisher<api::drive::Target>,
}

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

#[derive(phoxal::Service)]
#[phoxal(id = "slow-shutdown", api = y2026_1)]
struct SlowShutdown {}

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
                && c["topic"] == "drive/target"
                && c["direction"] == "publish"
        }),
        "emit-apis should report the drive/target publish contract"
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
                && c["topic"] == "drive/target"
                && c["direction"] == "publish"
        })
        .count();
    assert_eq!(publish_drive_target, 1);
    assert!(contracts.iter().any(|c| {
        c["api_version"] == "y2026_1"
            && c["topic"] == "map/revision"
            && c["direction"] == "subscribe"
    }));
    assert!(contracts.iter().any(|c| {
        c["api_version"] == "y2026_1"
            && c["topic"] == "map/submap"
            && c["direction"] == "query_request"
    }));
    assert!(contracts.iter().any(|c| {
        c["api_version"] == "y2026_1"
            && c["topic"] == "map/submap"
            && c["direction"] == "query_response"
    }));
}
