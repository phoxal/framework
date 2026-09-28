//! Public-session proof that the robot-owned conversion reaches Navigation.
//!
//! Build the bundle before running this test so Cargo does not recursively
//! compete for the same package lock.

use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use phoxal::communication::session::SupervisorState;
use phoxal::session::{ConnectionConfig, ObservationItem, connect};

phoxal::api!();

struct SupervisorProcess(Child);

impl Drop for SupervisorProcess {
    fn drop(&mut self) {
        let group = self.0.id() as i32;
        // SAFETY: this positive pid belongs to the process group we created
        // for the supervisor and its child runtimes.
        unsafe { libc::kill(-group, libc::SIGTERM) };
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "build the adapted robot bundle and set PHOXAL_ADAPTED_BUNDLE"]
async fn world_revision_reaches_navigation_through_generated_adapter() {
    let root = PathBuf::from(std::env::var_os("PHOXAL_ADAPTED_BUNDLE").expect("bundle path"));
    let mut command = Command::new(root.join("bin/supervisor"));
    command
        .arg(&root)
        .args(["--scope", "local", "--supervisor-id", "adapter-proof"])
        .args(["--launch-mode", "hardware"])
        .process_group(0);
    let mut supervisor = SupervisorProcess(command.spawn().expect("launch supervisor"));
    let endpoint = format!(
        "unixsock-stream/{}",
        root.join(".phoxal/run/supervisor.sock").display()
    );

    tokio::time::timeout(Duration::from_secs(30), async {
        let connection = loop {
            assert!(supervisor.0.try_wait().expect("poll supervisor").is_none());
            let config =
                ConnectionConfig::new(&endpoint, "local", "adapter-proof").expect("session config");
            match connect(config).await {
                Ok(connection) => break connection,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        };
        let session = connection
            .supervisor("adapter-proof")
            .await
            .expect("supervisor session");
        loop {
            let status = session.management().status().await.expect("status");
            match SupervisorState::try_from(status.state).unwrap_or_default() {
                SupervisorState::Ready => break,
                SupervisorState::Failed => panic!("supervisor failed: {:?}", status.detail),
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        let execution_id = loop {
            let executions = session.management().executions().await.expect("executions");
            if let Some(execution) = executions.first() {
                break execution.execution_id.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let execution = session
            .execution(&execution_id)
            .await
            .expect("select execution");
        let navigation = execution.service("navigation").await.expect("navigation");
        let mut status = navigation
            .method(api::navigation::status().method())
            .await
            .expect("status method")
            .observe()
            .await
            .expect("status observation");
        loop {
            match status
                .recv()
                .await
                .expect("observation stream")
                .expect("decode")
            {
                ObservationItem::Value { value, .. }
                    if value.map_revision.is_some_and(|revision| revision > 0) =>
                {
                    eprintln!(
                        "navigation observed converted world revision: {:?}",
                        value.map_revision
                    );
                    break;
                }
                ObservationItem::Value { .. } | ObservationItem::InitialAbsent { .. } => {}
                other => panic!("navigation observation ended before conversion: {other:?}"),
            }
        }
    })
    .await
    .expect("the live conversion reaches Navigation within 30 seconds");
}
