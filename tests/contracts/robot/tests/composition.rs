//! Cross-process composition proof for the manifest evaluation.
//!
//! Runs an already-built composition bundle (manifest consumer + independent
//! producer + this brain) under the real supervisor executable and drives
//! the generated operations through a public session.
//!
//! The bundle is passed with `PHOXAL_EVAL_BUNDLE` (built by
//! `cargo-phoxal build --output ...` outside this test; nesting cargo inside
//! cargo deadlocks on shared locks).  Without the variable the test exits
//! successfully so ordinary workspace runs stay green.  Every await is
//! bounded.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "acceptance test")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use phoxal::communication::session::SupervisorState;
use phoxal::contract::Empty;
use phoxal::session::{
    CallOutcome, Connection, ConnectionConfig, ObservationItem, Supervisor, connect,
};

phoxal::api!();

use api::consumer::ConsumerStatus;

const STARTUP: Duration = Duration::from_secs(30);
const SHUTDOWN: Duration = Duration::from_secs(15);
/// Bounded deadline for the composition to reach the expected call and
/// observation progress; supervisor readiness alone proves nothing about it.
const PROGRESS: Duration = Duration::from_secs(120);

struct SupervisorProcess {
    child: std::process::Child,
    group: i32,
}

impl SupervisorProcess {
    fn launch(bundle: &Path) -> Self {
        let mut command = Command::new(supervisor_executable());
        command
            .arg(bundle)
            .args(["--scope", "local", "--supervisor-id", "composition-e2e"]);
        // Hardware mode is the production transport for non-simulated robots;
        // controlled mode is selectable for the simulated-clock proof.
        let launch_mode = std::env::var("PHOXAL_LAUNCH_MODE")
            .ok()
            .filter(|mode| mode == "controlled" || mode == "hardware")
            .unwrap_or_else(|| "hardware".to_owned());
        command.args(["--launch-mode", &launch_mode]);
        let child = command.spawn().expect("launch supervisor executable");
        let pid = child.id() as i32;
        // SAFETY: move the supervisor into its own process group before any
        // runtime child can join it, so cleanup kills the whole tree.
        unsafe {
            libc::setpgid(pid, pid);
        }
        Self { child, group: pid }
    }

    fn is_finished(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("query supervisor exit")
            .is_some()
    }

    fn shutdown(&mut self) {
        // SAFETY: this positive PID belongs to the live child retained here.
        unsafe {
            libc::kill(self.group, libc::SIGTERM);
        }
        let deadline = std::time::Instant::now() + SHUTDOWN;
        while self
            .child
            .try_wait()
            .expect("poll supervisor exit")
            .is_none()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "supervisor ignored SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for SupervisorProcess {
    fn drop(&mut self) {
        // SAFETY: kill only this test's process group, including runtimes.
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("workspace root")
        .to_path_buf()
}

fn supervisor_executable() -> PathBuf {
    let path = workspace_root().join("target/debug/phoxal-supervisor");
    assert!(
        path.is_file(),
        "source supervisor must be built at {}",
        path.display()
    );
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "set PHOXAL_EVAL_BUNDLE to a compiled composition bundle; build it outside cargo to avoid nested-cargo deadlocks"]
async fn manifest_composition_exchanges_operations_and_observations_across_processes() {
    let Some(bundle) = std::env::var_os("PHOXAL_EVAL_BUNDLE").map(PathBuf::from) else {
        eprintln!(
            "skipping: PHOXAL_EVAL_BUNDLE is not set; build one with \
             `cargo-phoxal build --output <dir>` from tests/contracts/robot"
        );
        return;
    };
    let root = bundle.canonicalize().expect("bundle root canonicalizes");
    let socket = root.join(".phoxal/run/supervisor.sock");
    let endpoint = format!("unixsock-stream/{}", socket.display());

    let mut supervisor = SupervisorProcess::launch(&root);
    let connection = tokio::time::timeout(STARTUP, connect_when_bound(&endpoint, &mut supervisor))
        .await
        .expect("the supervisor binds its public session endpoint");
    let session = tokio::time::timeout(STARTUP, async {
        connection
            .supervisor("composition-e2e")
            .await
            .expect("the supervisor accepts the public session")
    })
    .await
    .expect("public session opens");
    tokio::time::timeout(STARTUP, wait_until_ready(&session, &mut supervisor))
        .await
        .expect("every runtime reaches Ready before the startup deadline")
        .expect("the composition stays healthy while reaching Ready");

    let execution_id = tokio::time::timeout(STARTUP, async {
        loop {
            let executions = session
                .management()
                .executions()
                .await
                .expect("execution inventory");
            if let Some(execution) = executions.first() {
                return execution.execution_id.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("an execution is admitted");
    let execution = tokio::time::timeout(STARTUP, session.execution(&execution_id))
        .await
        .expect("select execution")
        .expect("execution resolves");

    // 1. Consumer-initiated operation round trip: the consumer's inspect
    //    reply reflects completed read_encoder and read_backup calls against
    //    both providers.  Readiness does not imply progress: poll under a
    //    bounded deadline for the actual expected call completions, retaining
    //    intermediate values for diagnosis.
    let mut first_status: Option<ConsumerStatus> = None;
    let status = tokio::time::timeout(PROGRESS, async {
        loop {
            let consumer = execution
                .service("consumer")
                .await
                .expect("consumer service");
            let inspect = consumer
                .method(api::consumer::inspect(Empty {}).method())
                .await
                .expect("bind inspect");
            match inspect.call(Empty {}, STARTUP).await {
                Ok(CallOutcome::Received(status)) => {
                    let status: ConsumerStatus = status;
                    if first_status.is_none() {
                        first_status = Some(status.clone());
                        eprintln!("consumer status at first inspect: {status:?}");
                    }
                    if status.readings > 0
                        && status.backups > 0
                        && status.ticks > 0
                        && status.position_rad.is_some_and(|position| position < 1.0)
                        && status
                            .backup_position_rad
                            .is_some_and(|position| (10.0..11.0).contains(&position))
                    {
                        return status;
                    }
                }
                Ok(outcome) => panic!("inspect did not receive a reply: {outcome:?}"),
                Err(error) => panic!("inspect transport failed: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "composition did not reach expected progress within {:?}; first inspect saw {:?}",
            PROGRESS,
            first_status.expect("at least one inspect completed")
        )
    });
    assert!(
        status.phase == "running" || status.phase == "waiting",
        "the consumer reports a deliberate freshness phase, got {:?}",
        status.phase
    );
    eprintln!("consumer phase at progress: {:?}", status.phase);

    // 2. Observation exchange through the retained status output.
    let observed = tokio::time::timeout(STARTUP, async {
        let consumer = execution
            .service("consumer")
            .await
            .expect("consumer service");
        let mut observations = consumer
            .method(api::consumer::status().method())
            .await
            .expect("bind status observation")
            .observe()
            .await
            .expect("observe status");
        loop {
            match observations
                .recv()
                .await
                .expect("observation stream stays open")
                .expect("observation decodes")
            {
                ObservationItem::Value { value, .. } => break value,
                ObservationItem::InitialAbsent { .. } => continue,
                unexpected => panic!("unexpected observation record: {unexpected:?}"),
            }
        }
    })
    .await
    .expect("the retained status observation arrives");
    let observed: ConsumerStatus = observed;
    assert!(
        observed.observed > 0,
        "the published status carries live state: {observed:?}"
    );

    // 3. Robot-brain-initiated operation parity: the brain completes its own
    //    paced inspect calls and serves the tally.
    let tally = tokio::time::timeout(STARTUP * 2, async {
        loop {
            let brain = execution.service("brain").await.expect("brain service");
            let report = brain
                .method(api::brain::report(Empty {}).method())
                .await
                .expect("bind report");
            if let CallOutcome::Received(tally) = report
                .call(Empty {}, STARTUP)
                .await
                .expect("report call succeeds")
                && tally.inspect_completions > 0
            {
                return tally;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("the brain completes generated inspect calls");
    assert!(
        tally.last_phase == "running"
            || tally.last_phase == "waiting"
            || tally.last_phase.is_empty(),
        "the brain observed the consumer phase: {tally:?}"
    );

    // Clean termination is the supervisor's: sessions drop with the process.
    // (Closing the public session first can block indefinitely against a
    // hardware-mode supervisor; SIGTERM below is the production cleanup.)
    supervisor.shutdown();
}

async fn connect_when_bound(endpoint: &str, supervisor: &mut SupervisorProcess) -> Connection {
    loop {
        assert!(
            !supervisor.is_finished(),
            "the supervisor exited before it was reachable at {endpoint}"
        );
        let config = ConnectionConfig::new(endpoint, "local", "composition-e2e")
            .expect("the session config is valid");
        match connect(config).await {
            Ok(connection) => return connection,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}

async fn wait_until_ready(
    session: &Supervisor,
    supervisor: &mut SupervisorProcess,
) -> phoxal::Result<()> {
    loop {
        assert!(
            !supervisor.is_finished(),
            "the supervisor exited during admission"
        );
        let status = session.management().status().await?;
        match SupervisorState::try_from(status.state).unwrap_or_default() {
            SupervisorState::Ready => return Ok(()),
            SupervisorState::Failed => {
                return Err(phoxal::anyhow!(
                    "supervisor entered Failed before Ready: {:?}",
                    status.detail
                ));
            }
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "set PHOXAL_PROJECTION_BUNDLE to a compiled projection composition bundle"]
async fn foreign_observation_projects_into_the_standard_consumer() {
    let Some(bundle) = std::env::var_os("PHOXAL_PROJECTION_BUNDLE").map(PathBuf::from) else {
        eprintln!("skipping: PHOXAL_PROJECTION_BUNDLE is not set");
        return;
    };
    let root = bundle.canonicalize().expect("bundle root canonicalizes");
    let endpoint = format!(
        "unixsock-stream/{}",
        root.join(".phoxal/run/supervisor.sock").display()
    );

    let mut supervisor = SupervisorProcess::launch(&root);
    let connection = tokio::time::timeout(STARTUP, connect_when_bound(&endpoint, &mut supervisor))
        .await
        .expect("the supervisor binds its public session endpoint");
    let session = tokio::time::timeout(STARTUP, async {
        connection
            .supervisor("composition-e2e")
            .await
            .expect("the supervisor accepts the public session")
    })
    .await
    .expect("public session opens");
    tokio::time::timeout(STARTUP, wait_until_ready(&session, &mut supervisor))
        .await
        .expect("every runtime reaches Ready before the startup deadline")
        .expect("the composition stays healthy while reaching Ready");

    let execution_id = tokio::time::timeout(STARTUP, async {
        loop {
            let executions = session
                .management()
                .executions()
                .await
                .expect("execution inventory");
            if let Some(execution) = executions.first() {
                return execution.execution_id.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("an execution is admitted");
    let execution = tokio::time::timeout(STARTUP, session.execution(&execution_id))
        .await
        .expect("select execution")
        .expect("execution resolves");

    // The unchanged consumer must see foreign shaft values (base 5.0, wire
    // numbers 2/5) arrive as standard EncoderSample positions (wire numbers
    // 1/2) through the receiver-side projection.
    let mut first: Option<ConsumerStatus> = None;
    let status = tokio::time::timeout(PROGRESS, async {
        loop {
            let consumer = execution
                .service("consumer")
                .await
                .expect("consumer service");
            let inspect = consumer
                .method(api::consumer::inspect(Empty {}).method())
                .await
                .expect("bind inspect");
            if let Ok(CallOutcome::Received(status)) = inspect.call(Empty {}, STARTUP).await {
                let status: ConsumerStatus = status;
                if first.is_none() {
                    first = Some(status.clone());
                    eprintln!("projection status at first inspect: {status:?}");
                }
                if status.observed > 0
                    && status
                        .position_rad
                        .is_some_and(|position| (5.0..6.0).contains(&position))
                {
                    return status;
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "projection did not reach the consumer within {:?}; first inspect saw {:?}",
            PROGRESS,
            first.expect("at least one inspect completed")
        )
    });
    eprintln!(
        "projected observation position: {:?} (foreign producer base 5.0)",
        status.position_rad
    );
    supervisor.shutdown();
}

/// Sequential A/B provider substitution, live run B: the unchanged consumer
/// executable (digest-equal to run A's) must report provider B's values when
/// composition selects encoder-b's `encoder` output for the same endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "set PHOXAL_SUBSTITUTION_BUNDLE to a compiled substitution bundle (consumer.encoder <- encoder-b.encoder)"]
async fn provider_substitution_reports_provider_b_values() {
    let Some(bundle) = std::env::var_os("PHOXAL_SUBSTITUTION_BUNDLE").map(PathBuf::from) else {
        eprintln!("skipping: PHOXAL_SUBSTITUTION_BUNDLE is not set");
        return;
    };
    let root = bundle.canonicalize().expect("bundle root canonicalizes");
    let endpoint = format!(
        "unixsock-stream/{}",
        root.join(".phoxal/run/supervisor.sock").display()
    );

    let mut supervisor = SupervisorProcess::launch(&root);
    let connection = tokio::time::timeout(STARTUP, connect_when_bound(&endpoint, &mut supervisor))
        .await
        .expect("the supervisor binds its public session endpoint");
    let session = tokio::time::timeout(STARTUP, async {
        connection
            .supervisor("composition-e2e")
            .await
            .expect("the supervisor accepts the public session")
    })
    .await
    .expect("public session opens");
    tokio::time::timeout(STARTUP, wait_until_ready(&session, &mut supervisor))
        .await
        .expect("every runtime reaches Ready before the startup deadline")
        .expect("the composition stays healthy while reaching Ready");

    let execution_id = tokio::time::timeout(STARTUP, async {
        loop {
            let executions = session
                .management()
                .executions()
                .await
                .expect("execution inventory");
            if let Some(execution) = executions.first() {
                return execution.execution_id.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("an execution is admitted");
    let execution = tokio::time::timeout(STARTUP, session.execution(&execution_id))
        .await
        .expect("select execution")
        .expect("execution resolves");

    // Provider B (encoder-b) reports positions around 10.0 rad; run A's
    // encoder-a positions stay below 1.0, so the observed range identifies
    // the live provider through the unchanged consumer.
    let mut first: Option<ConsumerStatus> = None;
    let status = tokio::time::timeout(PROGRESS, async {
        loop {
            let consumer = execution
                .service("consumer")
                .await
                .expect("consumer service");
            let inspect = consumer
                .method(api::consumer::inspect(Empty {}).method())
                .await
                .expect("bind inspect");
            if let Ok(CallOutcome::Received(status)) = inspect.call(Empty {}, STARTUP).await {
                let status: ConsumerStatus = status;
                if first.is_none() {
                    first = Some(status.clone());
                    eprintln!("substitution status at first inspect: {status:?}");
                }
                if status.observed > 0
                    && status
                        .position_rad
                        .is_some_and(|position| (10.0..11.0).contains(&position))
                {
                    return status;
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "provider B values did not reach the consumer within {:?}; first inspect saw {:?}",
            PROGRESS,
            first.expect("at least one inspect completed")
        )
    });
    eprintln!(
        "substituted observation position: {:?} (provider B base 10.0)",
        status.position_rad
    );
    supervisor.shutdown();
}
