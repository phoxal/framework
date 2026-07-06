//! The runner loads the resolved robot model from a robot root and exposes it to
//! `#[setup]` via `SetupContext::robot()` / `robot_root()` (D33).

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use phoxal::participant::{ParticipantLaunch, RealClock, run_with};
use phoxal::prelude::*;
use serial_test::serial;

static SEEN: OnceLock<(String, String)> = OnceLock::new();

#[derive(phoxal::Service)]
#[phoxal(id = "reads-robot", api = y2026_1)]
struct ReadsRobot {}

#[phoxal::behavior]
impl ReadsRobot {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        // An official-style participant reads its typed state from the robot model.
        let robot = ctx.robot()?;
        let root = ctx.robot_root()?;
        let _ = SEEN.set((robot.manifest.robot.id.clone(), root.display().to_string()));
        Ok(Self {})
    }
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixture/robot/rgbd-imu-diff-drive")
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_reads_robot_model_from_the_root() {
    let fixture = fixture_dir();
    let launch = ParticipantLaunch::local("reads-robot-1", "rgbd-imu-diff-drive")
        .with_robot_root(fixture.clone());
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    run_with::<ReadsRobot, _, _>(launch, RealClock::new(), shutdown)
        .await
        .expect("runner should complete with a robot root");

    let (id, root) = SEEN.get().expect("setup should have read the robot model");
    assert_eq!(id, "rgbd-imu-diff-drive");
    assert!(root.ends_with("rgbd-imu-diff-drive"));
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn robot_is_absent_without_a_root() {
    // No robot root means `ctx.robot()` errors, so setup fails and the runner
    // surfaces the error.
    let launch = ParticipantLaunch::local("reads-robot-2", "robot");
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let result = run_with::<ReadsRobot, _, _>(launch, RealClock::new(), shutdown).await;
    assert!(
        result.is_err(),
        "setup should fail when no robot model is bound"
    );
}
