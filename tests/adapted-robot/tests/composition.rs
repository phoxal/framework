//! Public-session proofs that the robot-owned hosted conversion reaches
//! Navigation — and that hardware-mode delivery is arrival-aligned rather
//! than tick-delayed.
//!
//! Build the bundle before running these tests so Cargo does not
//! recursively compete for the same package lock.

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

/// Launches the bundle in hardware scheduling mode and connects a public
/// session, returning the pieces the proofs need.
async fn launch_hardware(
    supervisor_id: &str,
) -> (SupervisorProcess, phoxal::session::Execution, String) {
    let root = PathBuf::from(std::env::var_os("PHOXAL_ADAPTED_BUNDLE").expect("bundle path"));
    let mut command = Command::new(root.join("bin/supervisor"));
    command
        .arg(&root)
        .args(["--scope", "local", "--supervisor-id", supervisor_id])
        .args(["--launch-mode", "hardware"])
        .process_group(0);
    let mut supervisor = SupervisorProcess(command.spawn().expect("launch supervisor"));
    let endpoint = format!(
        "unixsock-stream/{}",
        root.join(".phoxal/run/supervisor.sock").display()
    );
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
    (supervisor, execution, execution_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "build the adapted robot bundle and set PHOXAL_ADAPTED_BUNDLE"]
async fn world_revision_reaches_navigation_through_generated_adapter() {
    let (mut supervisor, execution, _execution_id) =
        launch_hardware("adapter-proof").await;
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

/// The hardware scheduling proof: the conversion role forwards on input
/// arrival, not at its periodic release.
///
/// Every participant runs at a 20 ms cadence. The adapter's converted
/// MapState preserves the world capture stamp
/// (`oldest_capture_time_nanos`), so observing the producer's own
/// revision publication and the converter's output side by side pairs
/// samples by capture stamp; the per-stamp delta isolates exactly the
/// converter's contribution. Arrival-aligned forwarding adds transport
/// and conversion only (a couple of milliseconds); a tick-bound
/// converter would hold each sample for a uniform fraction of the 20 ms
/// period (median near half a period, tail near a full period).
/// Shutdown then holds through the arrival-driven path: the supervisor's
/// termination stops the whole process group.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "build the adapted robot bundle and set PHOXAL_ADAPTED_BUNDLE"]
async fn hardware_conversion_forwards_on_arrival_within_the_period() {
    const PERIOD_MS: u64 = 20;
    const SAMPLES: usize = 40;
    // The hosted conversion role's navigation.map edge is `target_1`; its
    // descriptor matches the record the executable retains.
    const ADAPTER_MAP: phoxal::contracts::ObservationMethod<
        api::types::phoxal::private::phoxal_2dservice_2dnavigation::phoxal_5fservice_5fnavigation::contract::MapState,
    > = phoxal::contracts::ObservationMethod::new(
        "phoxal.private.phoxal_2dservice_2dnavigation.phoxal_5fservice_5fnavigation.contract.MapState",
        "target_1",
        "target_1",
        "google.protobuf.Empty",
        "phoxal.private.phoxal_2dservice_2dnavigation.phoxal_5fservice_5fnavigation.contract.MapState",
        true,
        None,
        &[],
    );
    let (mut supervisor, execution, _execution_id) =
        launch_hardware("arrival-proof").await;
    let adapter = execution
        .service("phoxal-adapter")
        .await
        .expect("the hosted conversion instance is a session-visible service");
    let world = execution.service("world").await.expect("world service");
    let revision_handle = world
        .method(api::world::revision().method())
        .await
        .expect("world revision method");
    let map_handle = adapter
        .method(ADAPTER_MAP)
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
    .expect("the arrival window completes within 60 seconds");
    deltas_ms.sort_unstable();
    let median = deltas_ms[deltas_ms.len() / 2];
    let within_half_period = deltas_ms
        .iter()
        .filter(|delta| **delta <= PERIOD_MS / 4)
        .count();
    eprintln!("converter path deltas (ms): {deltas_ms:?}");
    assert!(
        median <= 4,
        "the median converter path delay is arrival-aligned, not tick-bound (median {median} ms)"
    );
    assert!(
        within_half_period * 5 >= deltas_ms.len() * 4,
        "at least 80% of converted deliveries trail their producer receipt by a quarter period or less ({within_half_period}/{} do)",
        deltas_ms.len()
    );
    // Stop/reset through the arrival-driven path: terminating the
    // supervisor stops its whole process group promptly.
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
