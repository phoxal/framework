//! Public-session proofs of canonical brain conversion and shutdown.
//! Build the bundle before running these tests to avoid nested Cargo locks.

use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use phoxal::communication::session::SupervisorState;
use phoxal::session::{ConnectionConfig, ObservationItem, connect};

phoxal::api!();

struct SupervisorProcess(Child, tempfile::TempDir);

impl Drop for SupervisorProcess {
    fn drop(&mut self) {
        let group = self.0.id() as i32;
        // SAFETY: this positive pid belongs to the process group we created
        // for the supervisor and its child runtimes.
        unsafe { libc::kill(-group, libc::SIGTERM) };
        let _ = self.0.wait();
    }
}

/// Launches the bundle in hardware scheduling mode and connects a public
/// session, returning the pieces the proofs need.
async fn launch_hardware(
    supervisor_id: &str,
) -> (SupervisorProcess, phoxal::session::Execution, String) {
    let root = PathBuf::from(std::env::var_os("PHOXAL_CONVERSION_BUNDLE").expect("bundle path"));
    let state_dir = tempfile::tempdir().expect("execution state directory");
    let mut command = Command::new(root.join("bin/supervisor"));
    command
        .arg(&root)
        .arg("--state-dir")
        .arg(state_dir.path())
        .args(["--scope", "local", "--supervisor-id", supervisor_id])
        .args(["--launch-mode", "hardware"])
        .process_group(0);
    let mut supervisor = SupervisorProcess(command.spawn().expect("launch supervisor"), state_dir);
    let endpoint = format!(
        "unixsock-stream/{}",
        supervisor.1.path().join("supervisor.sock").display()
    );
    let (execution, execution_id) = tokio::time::timeout(Duration::from_secs(30), async {
        let connection = loop {
            assert!(supervisor.0.try_wait().expect("poll supervisor").is_none());
            let config =
                ConnectionConfig::new(&endpoint, "local", supervisor_id).expect("session config");
            match connect(config).await {
                Ok(connection) => break connection,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        };
        let session = connection
            .supervisor(supervisor_id)
            .await
            .expect("supervisor session");
        loop {
            let status = session.management().status().await.expect("status");
            match status.state {
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
        (execution, execution_id)
    })
    .await
    .expect("supervisor session and execution become ready within 30 seconds");
    (supervisor, execution, execution_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "build the conversion robot bundle and set PHOXAL_CONVERSION_BUNDLE"]
async fn world_revision_reaches_navigation_through_canonical_brain() {
    let (mut supervisor, execution, _execution_id) = launch_hardware("conversion-proof").await;
    let navigation = execution.service("navigation").await.expect("navigation");
    let mut status = navigation
        .method(api::navigation::status().method())
        .await
        .expect("status method")
        .observe()
        .await
        .expect("status observation");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            assert!(supervisor.0.try_wait().expect("poll supervisor").is_none());
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

/// The brain forwards at its ordinary 20 ms cadence. Match producer and
/// converted samples by original capture time and verify delivery within
/// the consumer's existing 100 ms capture-age bound, then terminate the
/// execution and verify that its process group exits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "build the conversion robot bundle and set PHOXAL_CONVERSION_BUNDLE"]
async fn hardware_conversion_preserves_capture_and_stops_with_the_execution() {
    const CAPTURE_AGE_MS: u64 = 100;
    const SAMPLES: usize = 40;
    const MAP_WIRE: &str = <api::navigation::MapState as phoxal::schema::MessageSchema>::WIRE_NAME;
    const CONVERTED_MAP: phoxal::contracts::ObservationMethod<api::navigation::MapState> =
        phoxal::contracts::ObservationMethod::new(
            MAP_WIRE,
            "phoxal_conversion_out_0",
            "phoxal_conversion_out_0",
            "google.protobuf.Empty",
            MAP_WIRE,
            true,
            None,
            &[],
        );
    let (mut supervisor, execution, _execution_id) =
        launch_hardware("conversion-capture-proof").await;
    let brain = execution
        .service("brain")
        .await
        .expect("canonical brain runtime");
    let world = execution.service("world").await.expect("world service");
    let revision_handle = world
        .method(api::world::revision().method())
        .await
        .expect("world revision method");
    let map_handle = brain
        .method(CONVERTED_MAP)
        .await
        .expect("conversion target method");
    let mut revision = revision_handle
        .observe()
        .await
        .expect("world revision observation");
    let mut map = map_handle
        .observe()
        .await
        .expect("converted map observation");
    let mut raw_receipts: std::collections::BTreeMap<u64, std::time::Instant> =
        std::collections::BTreeMap::new();
    let mut deltas_ms: Vec<u64> = Vec::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        while deltas_ms.len() < SAMPLES {
            assert!(supervisor.0.try_wait().expect("poll supervisor").is_none());
            tokio::select! {
                item = revision.recv() => match item {
                    Some(Ok(ObservationItem::Value { value, .. })) => {
                        if let Some(capture) = value.oldest_capture_time_nanos {
                            raw_receipts.insert(capture, std::time::Instant::now());
                            while raw_receipts.len() > 128 {
                                let oldest = *raw_receipts
                                    .keys()
                                    .next()
                                    .expect("non-empty receipt window");
                                raw_receipts.remove(&oldest);
                            }
                        }
                    }
                    // A bounded client queue reports loss explicitly; the
                    // observer re-subscribes and keeps collecting.
                    Some(Err(error)) if error.to_string().contains("overflowed") => {
                        drop(revision);
                        revision = revision_handle
                            .observe()
                            .await
                            .expect("world revision re-observation");
                    }
                    Some(Err(error)) => panic!("world observation failed: {error}"),
                    Some(Ok(_)) => {}
                    None => panic!("world observation stream ended"),
                },
                item = map.recv() => match item {
                    Some(Ok(ObservationItem::Value { value, .. })) => {
                        let Some(capture) = value.oldest_capture_time_nanos else {
                            continue;
                        };
                        // The converter may republish the current sample;
                        // measure each distinct capture once, and only when
                        // its producer-side receipt is known.
                        let Some(received) = raw_receipts.remove(&capture) else {
                            continue;
                        };
                        deltas_ms.push(
                            received
                                .elapsed()
                                .as_millis()
                                .try_into()
                                .unwrap_or(u64::MAX),
                        );
                    }
                    Some(Err(error)) if error.to_string().contains("overflowed") => {
                        drop(map);
                        map = map_handle
                            .observe()
                            .await
                            .expect("converted map re-observation");
                    }
                    Some(Err(error)) => panic!("conversion observation failed: {error}"),
                    Some(Ok(_)) => {}
                    None => panic!("conversion observation stream ended"),
                },
            }
        }
    })
    .await
    .expect("the capture window completes within 60 seconds");
    deltas_ms.sort_unstable();
    eprintln!("canonical conversion path deltas (ms): {deltas_ms:?}");
    assert!(
        deltas_ms.iter().all(|delta| *delta <= CAPTURE_AGE_MS),
        "converted samples must arrive within Navigation's existing capture-age bound"
    );
    // Terminating the execution stops the brain and its providers together.
    let group = supervisor.0.id() as i32;
    // SAFETY: this positive pid belongs to the process group we created
    // for the supervisor and its child runtimes.
    unsafe { libc::kill(-group, libc::SIGTERM) };
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match supervisor.0.try_wait().expect("poll supervisor") {
            Some(_status) => break,
            None => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the supervisor process group did not exit after termination"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}
