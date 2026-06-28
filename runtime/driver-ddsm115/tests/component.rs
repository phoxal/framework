//! The runner launches the driver against a specific component instance, which
//! `#[setup]` reads via `ctx.component()` and binds to its per-component topics.

use std::path::PathBuf;
use std::time::Duration;

use phoxal::runtime::{ParticipantLaunch, RealClock, run_with};
use serial_test::serial;

// Re-run the driver binary's module as part of this integration test is not
// possible; instead we drive the runtime through the public runner with a launch
// that names a component instance, and assert setup succeeds (binds the instance)
// or fails (unknown instance). The driver type lives in the binary crate, so this
// test exercises it via a minimal local copy is unnecessary — we test the runner
// contract using the same fixture the binary would see.
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixture/robot/rgbd-imu-diff-drive")
}

// A tiny runtime that mirrors the driver's setup contract: read `ctx.component()`
// and validate it against the model. (The binary's `Ddsm115` is not importable
// from an integration test, so we assert the same `ctx.component()` + model wiring
// the driver relies on.)
mod driver_like {
    use phoxal::prelude::*;

    #[derive(phoxal::Runtime)]
    #[phoxal(id = "driver-like", api = y2026_1)]
    pub struct DriverLike {}

    #[phoxal::runtime]
    impl DriverLike {
        #[setup]
        async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
            let instance = ctx.component()?;
            // The bound instance must exist in the robot model.
            ctx.robot()?.component_instance(instance)?;
            Ok(Self {})
        }
    }
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn driver_binds_a_known_component_instance() {
    let launch = ParticipantLaunch::local("ddsm115-front_left_drive", "rgbd-imu-diff-drive")
        .with_bundle_root(fixture())
        .with_component_instance("front_left_drive");
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    run_with::<driver_like::DriverLike, _, _>(launch, RealClock::new(), shutdown)
        .await
        .expect("driver should bind a known component instance");
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn driver_rejects_an_unknown_component_instance() {
    let launch = ParticipantLaunch::local("ddsm115-nope", "rgbd-imu-diff-drive")
        .with_bundle_root(fixture())
        .with_component_instance("does-not-exist");
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    let result =
        run_with::<driver_like::DriverLike, _, _>(launch, RealClock::new(), shutdown).await;
    assert!(
        result.is_err(),
        "an unknown component instance must fail setup"
    );
}
