//! Runner integration: a scheduled runtime runs steps and then shuts down
//! cleanly, and its `emit-apis` document matches the frozen schema (D50).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;
use phoxal::runtime::{ParticipantLaunch, RealClock, emit_apis_json, run_with};

static STEPS: AtomicU64 = AtomicU64::new(0);
static SHUTDOWN_CALLED: AtomicBool = AtomicBool::new(false);

#[derive(phoxal::Runtime)]
#[phoxal(id = "counter", api = y2026_1)]
struct Counter {
    target: Publisher<api::drive::Target>,
}

#[phoxal::runtime]
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

#[test]
fn emit_apis_reports_frozen_schema() {
    let json = emit_apis_json::<Counter>();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(value["schema"], "phoxal.emit-apis/v0");
    assert_eq!(value["artifact"]["kind"], "runtime");
    assert_eq!(value["artifact"]["id"], "counter");
    assert_eq!(value["api_version"], "y2026_1");
    assert_eq!(value["bus_abi"], "phoxal-bus/v0");

    let contracts = value["required_contracts"].as_array().unwrap();
    assert!(
        contracts
            .iter()
            .any(|c| c["topic"] == "drive/target" && c["direction"] == "publish"),
        "emit-apis should report the drive/target publish contract"
    );
}
