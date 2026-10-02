//! Authored-runtime acceptance through the actual supervisor executable,
//! Zenoh transport, and the standalone countdown provider binary.
//!
//! Build both external binaries first (the harness build does not build
//! other packages' binaries):
//!
//! ```sh
//! cargo build --locked -p phoxal-supervisor --bin phoxal-supervisor
//! cargo build --locked -p phoxal-countdown-fixture
//! cargo build --locked -p phoxal-countdown-brain-fixture
//! cargo test --locked -p phoxal-supervisor-runtime-fixture -p phoxal-supervisor --features phoxal-supervisor-runtime-fixture/test-fixtures
//! ```

#![allow(clippy::expect_used, clippy::unwrap_used)]
#![recursion_limit = "256"]

mod support;

#[path = "../../runtime-authoring/consumer/src/contract.rs"]
mod brain_contract;

// The consumer contract names its provider payload through this module, so
// the include mirrors the consumer crate's own module layout.
#[path = "../../runtime-authoring/consumer/src/provider.rs"]
mod provider;

#[path = "../../runtime-authoring/consumer/src/expected_record.rs"]
mod brain_expected_record;

/// The generated provider operation marker the brain's contract names,
/// mirrored here with the same served identity so the included contract
/// compiles outside the consumer crate.
pub struct Start;
impl phoxal::contracts::Operation for Start {
    type Request = provider::StartRequest;
    type Response = provider::StartResponse;
    const METHOD: phoxal::contracts::CallMethod<Self::Request, Self::Response> =
        phoxal::contracts::CallMethod::new(
            "phoxal.tests.authoring.countdown.v1.Start",
            "start",
            "start",
            "phoxal.tests.authoring.countdown.v1.StartRequest",
            "phoxal.tests.authoring.countdown.v1.StartResponse",
            None,
            &[],
        );
}

use provider as contract;

#[path = "../../runtime-authoring/countdown/src/expected_record.rs"]
mod expected_record;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use brain_contract::{ConsumedEventState, brain_api};
use contract::{
    CancelRequest, CountdownState, Outcome, StartRequest, StartResponse, countdown_api,
};
use phoxal::communication::session::SupervisorState;
use phoxal::session::{
    CallOutcome, Connection, ConnectionConfig, ObservationItem, Supervisor, connect,
};

/// Slack for the supervisor to bind, admit the runtime, and reach Ready.
const STARTUP: Duration = Duration::from_secs(20);
/// Budget for wall-clock countdown durations (3 s plus cancellation of a
/// 0.5 s job) observed through the transport.
const COMPLETION: Duration = Duration::from_secs(15);
/// Bounded stability window after cancellation delivery: the cancelled
/// job's original 500 ms deadline plus transport slack.
const CANCELLATION_STABILITY_WINDOW: Duration = Duration::from_millis(1_500);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_authored_countdown_runtime_serves_real_transport_traffic() {
    let bundle = build_bundle();
    let root = bundle.root.canonicalize().expect("bundle root resolves");
    let socket = root
        .parent()
        .expect("the bundle has an owning release root")
        .join(".phoxal/run/supervisor.sock");
    let endpoint = format!("unixsock-stream/{}", socket.display());

    let mut supervisor = support::SupervisorProcess::launch(&root, "countdown-e2e");

    let connection = tokio::time::timeout(STARTUP, connect_when_bound(&endpoint, &mut supervisor))
        .await
        .expect("the supervisor binds its public session endpoint");
    let supervisor_session = connection
        .supervisor("countdown-e2e")
        .await
        .expect("the supervisor accepts the public session");
    tokio::time::timeout(STARTUP, wait_until_ready(&supervisor_session))
        .await
        .expect("the authored runtime reaches Ready")
        .expect("the authored runtime remains healthy while reaching Ready");

    let executions = supervisor_session
        .management()
        .executions()
        .await
        .expect("execution inventory");
    let execution_id = executions
        .first()
        .expect("one admitted execution")
        .execution_id
        .clone();
    let execution = supervisor_session
        .execution(&execution_id)
        .await
        .expect("select the admitted execution");
    // The countdown provider occupies the service slot; the authored brain
    // consumer owns the required brain slot and receives finished events
    // through the robot graph's connection.
    let countdown = execution
        .service("countdown")
        .await
        .expect("select the countdown service instance");
    let brain = execution
        .service("brain")
        .await
        .expect("select the brain consumer instance");
    let handled = brain
        .method(brain_api::HANDLED)
        .await
        .expect("bind the consumer's handled-state method");
    let mission = brain
        .method(brain_api::MISSION)
        .await
        .expect("bind the consumer's mission-state method");

    let status = countdown
        .method(countdown_api::STATUS)
        .await
        .expect("bind the status observation method");

    // The retained status stream exposes the authored initial state before
    // any request is served. This observes that the initial value is
    // reachable through the transport; it does not by itself prove the
    // publication preceded the first periodic step (periodic steps are
    // already running by the time the session observes) — that ordering is
    // proven by the runner bootstrap test
    // (`authored_bootstrap_publishes_initial_state_and_no_setpoint_before_
    // any_step`), which receives the initializer's publication before any
    // poll. Each read opens a fresh observation: the retained
    // value replays to new subscribers, so no held subscription can
    // overflow while the test awaits calls or deadlines. The helper
    // retains the public revision and payload: the record exposes a
    // revision and the decoded value, not a capture-time field, so
    // capture-time fidelity is unobservable through this tier.
    let initial = current_status(&status).await;
    assert_eq!(
        initial.value.active_job_id, None,
        "no job is active initially"
    );
    assert_eq!(
        initial.value.last_job_id, None,
        "no job has finished initially"
    );
    trace(
        "initial:countdown",
        initial.revision,
        format!(
            "active={:?} last={:?}",
            initial.value.active_job_id, initial.value.last_job_id
        ),
    );
    let initial_consumed = current_consumed(&handled).await;
    assert_eq!(
        (
            initial_consumed.value.last_job_id,
            initial_consumed.value.handled_count
        ),
        (None, 0),
        "the consumer has handled no events initially"
    );
    trace(
        "initial:consumer",
        initial_consumed.revision,
        format!(
            "last={:?} outcome={:?} count={}",
            initial_consumed.value.last_job_id,
            initial_consumed.value.last_outcome,
            initial_consumed.value.handled_count
        ),
    );

    let start = countdown
        .method(countdown_api::START)
        .await
        .expect("bind the start call method");
    let cancel = countdown
        .method(countdown_api::CANCEL)
        .await
        .expect("bind the cancel call method");

    // A valid start is accepted with its typed correlated response, and the
    // retained status then identifies that job.
    let outcome = start
        .call(
            StartRequest {
                job_id: 1,
                duration_ms: 3_000,
            },
            STARTUP,
        )
        .await
        .expect("start crosses the transport");
    assert!(
        matches!(outcome, CallOutcome::Received(response) if matches!(response, StartResponse::Accepted)),
        "job 1 is accepted, got {outcome:?}"
    );
    eprintln!("[trace] start(job 1) -> Accepted");
    let observed = wait_for_status(&status, |state| state.active_job_id == Some(1)).await;
    assert_eq!(observed.value.last_job_id, None);
    trace(
        "active:countdown",
        observed.revision,
        format!(
            "active={:?} last={:?}",
            observed.value.active_job_id, observed.value.last_job_id
        ),
    );

    // A second start while one is active is refused as Busy, and a cancel
    // for a job that never ran reflects the actual state as UnknownJob.
    let outcome = start
        .call(
            StartRequest {
                job_id: 2,
                duration_ms: 500,
            },
            STARTUP,
        )
        .await
        .expect("concurrent start crosses the transport");
    assert!(
        matches!(outcome, CallOutcome::Received(response) if matches!(response, StartResponse::Busy)),
        "job 2 is refused while job 1 is active, got {outcome:?}"
    );
    eprintln!("[trace] start(job 2) -> Busy");
    let outcome = cancel
        .call(CancelRequest { job_id: 2 }, STARTUP)
        .await
        .expect("unknown-job cancel crosses the transport");
    assert!(
        matches!(outcome, CallOutcome::Received(response) if matches!(response, contract::CancelResponse::UnknownJob)),
        "an unknown job reports UnknownJob, got {outcome:?}"
    );
    eprintln!("[trace] cancel(job 2) -> UnknownJob");

    // Refusals fabricate no events: after Busy and UnknownJob the consumer
    // still has handled nothing.
    let refused = current_consumed(&handled).await;
    assert_eq!(
        (refused.value.last_job_id, refused.value.handled_count),
        (None, 0),
        "Busy and UnknownJob refusals produce no finished events"
    );
    trace(
        "refused:consumer",
        refused.revision,
        format!(
            "last={:?} outcome={:?} count={}",
            refused.value.last_job_id, refused.value.last_outcome, refused.value.handled_count
        ),
    );

    // Completion at the deadline surfaces through the retained status: the
    // job stops being active and its terminal result is recorded. The
    // finished event itself is a transient queue output, which the public
    // session observation surface does not carry (it exposes retained
    // values only); its delivery is proven below through the connected
    // consumer's own retained state, never through a session subscription
    // of the transient port.
    let completed = wait_for_status(&status, |state| {
        state.active_job_id.is_none() && state.last_job_id == Some(1)
    })
    .await;
    assert!(matches!(completed.value.last_outcome, Outcome::Completed));
    trace(
        "completed:countdown",
        completed.revision,
        format!(
            "active={:?} last={:?}",
            completed.value.active_job_id, completed.value.last_job_id
        ),
    );

    // The real finished event is DELIVERED through the supervisor graph to
    // the connected consumer, observed only through the consumer's own
    // retained state reaching job 1 / Completed / count 1.
    let consumed = wait_for_consumed(&handled, |state| state.handled_count == 1).await;
    assert_eq!(consumed.value.last_job_id, Some(1));
    assert!(matches!(
        consumed.value.last_outcome,
        Some(provider::Outcome::Completed)
    ));
    trace(
        "completed:consumer",
        consumed.revision,
        format!(
            "last={:?} outcome={:?} count={}",
            consumed.value.last_job_id, consumed.value.last_outcome, consumed.value.handled_count
        ),
    );

    // The finished job's identifier cannot be reused immediately.
    let outcome = start
        .call(
            StartRequest {
                job_id: 1,
                duration_ms: 100,
            },
            STARTUP,
        )
        .await
        .expect("reuse attempt crosses the transport");
    assert!(
        matches!(outcome, CallOutcome::Received(response) if matches!(response, StartResponse::Invalid)),
        "immediate reuse of job 1 is invalid, got {outcome:?}"
    );
    eprintln!("[trace] start(job 1 reuse) -> Invalid");

    // A short job that is cancelled in flight reports cancellation through
    // the typed response and the retained status, without assuming that the
    // earlier acceptance implies completion.
    let outcome = start
        .call(
            StartRequest {
                job_id: 3,
                duration_ms: 500,
            },
            STARTUP,
        )
        .await
        .expect("job 3 start crosses the transport");
    assert!(
        matches!(outcome, CallOutcome::Received(response) if matches!(response, StartResponse::Accepted)),
        "job 3 is accepted, got {outcome:?}"
    );
    eprintln!("[trace] start(job 3) -> Accepted");
    let outcome = cancel
        .call(CancelRequest { job_id: 3 }, STARTUP)
        .await
        .expect("job 3 cancel crosses the transport");
    assert!(
        matches!(outcome, CallOutcome::Received(response) if matches!(response, contract::CancelResponse::Cancelled)),
        "the active job 3 is cancelled, got {outcome:?}"
    );
    eprintln!("[trace] cancel(job 3) -> Cancelled");
    let cancelled = wait_for_status(&status, |state| {
        state.active_job_id.is_none() && state.last_job_id == Some(3)
    })
    .await;
    assert!(matches!(cancelled.value.last_outcome, Outcome::Cancelled));
    trace(
        "cancelled:countdown",
        cancelled.revision,
        format!(
            "active={:?} last={:?}",
            cancelled.value.active_job_id, cancelled.value.last_job_id
        ),
    );

    // The cancellation event reaches count 2; the bounded window below
    // checks for later duplicate or completion delivery.
    let consumed_cancel = wait_for_consumed(&handled, |state| state.handled_count == 2).await;
    assert_eq!(consumed_cancel.value.last_job_id, Some(3));
    assert!(matches!(
        consumed_cancel.value.last_outcome,
        Some(provider::Outcome::Cancelled)
    ));
    trace(
        "cancelled:consumer",
        consumed_cancel.revision,
        format!(
            "last={:?} outcome={:?} count={}",
            consumed_cancel.value.last_job_id,
            consumed_cancel.value.last_outcome,
            consumed_cancel.value.handled_count
        ),
    );

    // A delayed duplicate cancellation event, or a late completion of the
    // cancelled job, would surface in the consumer's state. Hold a bounded
    // window covering job 3's original 500 ms deadline plus transport
    // slack and require the consumer's history to stay exactly at job 3 /
    // Cancelled / count 2, extended only by the brain mission's own single
    // job-42 completion (last 42 / Completed / count 3). Any other count
    // or identity — a redelivered cancellation, a duplicate mission event,
    // or a late completion of job 3 — fails promptly. This proves no
    // duplicate within the tested window — not an unbounded exactly-once
    // guarantee.
    let window = std::time::Instant::now();
    while window.elapsed() < CANCELLATION_STABILITY_WINDOW {
        let stable = current_consumed(&handled).await;
        match (stable.value.handled_count, stable.value.last_job_id) {
            (2, Some(3)) => assert!(matches!(
                stable.value.last_outcome,
                Some(provider::Outcome::Cancelled)
            )),
            (3, Some(42)) => assert!(
                matches!(
                    stable.value.last_outcome,
                    Some(provider::Outcome::Completed)
                ),
                "the mission's own completion is the only allowed third event"
            ),
            (count, last) => panic!(
                "no duplicate or unexpected event within the tested window: \
                 count {count}, last {last:?}"
            ),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let final_consumed = current_consumed(&handled).await;
    match (
        final_consumed.value.handled_count,
        final_consumed.value.last_job_id,
    ) {
        (2, Some(3)) => assert!(matches!(
            final_consumed.value.last_outcome,
            Some(provider::Outcome::Cancelled)
        )),
        (3, Some(42)) => assert!(matches!(
            final_consumed.value.last_outcome,
            Some(provider::Outcome::Completed)
        )),
        (count, last) => {
            panic!("the stable window ended on an unexpected history: count {count}, last {last:?}")
        }
    }
    // Once the test's own jobs clear, the tree's mission starts its own
    // job against the idle countdown, observes completion through the
    // retained status, holds its bounded delay, and latches success.
    let deadline = std::time::Instant::now() + COMPLETION * 4;
    let mut mission_phase = current_mission(&mission).await as i32;
    while !matches!(mission_phase, 2..=6) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        mission_phase = current_mission(&mission).await as i32;
    }
    assert_eq!(
        mission_phase, 2,
        "the mission reaches Succeeded through the graph"
    );

    trace(
        "stable-window-end:consumer",
        final_consumed.revision,
        format!(
            "last={:?} outcome={:?} count={}",
            final_consumed.value.last_job_id,
            final_consumed.value.last_outcome,
            final_consumed.value.handled_count
        ),
    );

    tokio::time::timeout(STARTUP, supervisor_session.close())
        .await
        .expect("the supervisor session closes in time")
        .expect("the supervisor session closes cleanly");
    tokio::time::timeout(STARTUP, connection.close())
        .await
        .expect("the public connection closes in time")
        .expect("the public connection closes cleanly");
    supervisor.shutdown().await;
}

/// Connect as soon as the supervisor's embedded router is listening.
async fn connect_when_bound(
    endpoint: &str,
    supervisor: &mut support::SupervisorProcess,
) -> Connection {
    loop {
        assert!(
            !supervisor.is_finished(),
            "the supervisor exited before it was reachable at {endpoint}"
        );
        let config = ConnectionConfig::new(endpoint, "local", "countdown-e2e")
            .expect("the session config is valid");
        match connect(config).await {
            Ok(connection) => return connection,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}

/// Wait for the public supervisor projection to observe Runtime admission.
async fn wait_until_ready(supervisor: &Supervisor) -> phoxal::Result<()> {
    loop {
        let status = supervisor.management().status().await?;
        match status.state {
            state if state == SupervisorState::Ready as i32 => return Ok(()),
            state if state == SupervisorState::Failed as i32 => {
                anyhow::bail!(
                    "supervisor entered Failed before Runtime Ready: {:?}",
                    status.detail
                )
            }
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

/// One received retained observation: the public record's revision and the
/// decoded value. The public schema carries no capture-time field.
struct Observed<T> {
    revision: u64,
    value: T,
}

/// Emits one bounded trace checkpoint with the actual received revision and
/// value; checkpoints are named events, not every periodic observation.
fn trace(checkpoint: &str, revision: u64, detail: String) {
    eprintln!("[trace] {checkpoint} rev={revision} {detail}");
}

/// Reads the retained status value from one fresh observation, skipping
/// the stream's initial absence record and failing with the exact record
/// for anything else.
async fn current_status(
    status: &phoxal::session::ObservationHandle<CountdownState>,
) -> Observed<CountdownState> {
    let mut observations = tokio::time::timeout(COMPLETION, status.observe())
        .await
        .expect("the observation subscription starts")
        .expect("observe status");
    let value = tokio::time::timeout(COMPLETION, async {
        loop {
            match observations.recv().await {
                Some(Ok(ObservationItem::Value {
                    value, revision, ..
                })) => return Observed { revision, value },
                Some(Ok(ObservationItem::InitialAbsent { .. })) => continue,
                Some(Ok(other)) => panic!("unexpected status record: {other:?}"),
                Some(Err(error)) => panic!("the status observation failed: {error:?}"),
                None => panic!("the status stream ended before a value"),
            }
        }
    })
    .await
    .expect("a retained status value arrives");
    drop(observations);
    value
}

/// Reads the consumer's retained handled-state value from one fresh
/// observation, skipping the stream's initial absence record.
async fn current_consumed(
    handled: &phoxal::session::ObservationHandle<ConsumedEventState>,
) -> Observed<ConsumedEventState> {
    let mut observations = tokio::time::timeout(COMPLETION, handled.observe())
        .await
        .expect("the handled observation starts")
        .expect("observe handled state");
    let value = tokio::time::timeout(COMPLETION, async {
        loop {
            match observations.recv().await {
                Some(Ok(ObservationItem::Value {
                    value, revision, ..
                })) => return Observed { revision, value },
                Some(Ok(ObservationItem::InitialAbsent { .. })) => continue,
                Some(Ok(other)) => panic!("unexpected handled record: {other:?}"),
                Some(Err(error)) => panic!("the handled observation failed: {error:?}"),
                None => panic!("the handled stream ended before a value"),
            }
        }
    })
    .await
    .expect("a retained handled value arrives");
    drop(observations);
    value
}

/// Reads the mission's retained phase from one fresh observation.
async fn current_mission(
    mission: &phoxal::session::ObservationHandle<brain_contract::MissionState>,
) -> brain_contract::MissionPhase {
    let mut observations = tokio::time::timeout(COMPLETION, mission.observe())
        .await
        .expect("the mission observation starts")
        .expect("observe mission state");
    let phase = tokio::time::timeout(COMPLETION, async {
        loop {
            match observations.recv().await {
                Some(Ok(ObservationItem::Value { value, .. })) => return value.phase,
                Some(Ok(ObservationItem::InitialAbsent { .. })) => continue,
                Some(Ok(_)) => return brain_contract::MissionPhase::Unspecified,
                Some(Err(error)) => panic!("the mission observation failed: {error:?}"),
                None => return brain_contract::MissionPhase::Unspecified,
            }
        }
    })
    .await
    .expect("a mission value arrives");
    drop(observations);
    phase
}

/// Waits until the consumer's retained handled state satisfies the
/// predicate.
async fn wait_for_consumed(
    handled: &phoxal::session::ObservationHandle<ConsumedEventState>,
    predicate: impl Fn(&ConsumedEventState) -> bool,
) -> Observed<ConsumedEventState> {
    tokio::time::timeout(COMPLETION, async {
        loop {
            let observed = current_consumed(handled).await;
            if predicate(&observed.value) {
                return observed;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the expected handled state arrives")
}

/// Waits until the retained status satisfies the predicate.
async fn wait_for_status(
    status: &phoxal::session::ObservationHandle<CountdownState>,
    predicate: impl Fn(&CountdownState) -> bool,
) -> Observed<CountdownState> {
    tokio::time::timeout(COMPLETION, async {
        loop {
            let observed = current_status(status).await;
            if predicate(&observed.value) {
                return observed;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the expected status value arrives")
}

/// Locates the compiled authored brain consumer fixture beside this
/// package's Runtime fixture binary: build it explicitly first
/// (`cargo build --locked -p phoxal-countdown-brain-fixture`).
fn consumer_binary() -> PathBuf {
    let path = std::path::Path::new(env!("CARGO_BIN_EXE_supervisor-test-runtime"))
        .parent()
        .expect("the Runtime fixture executable has a parent directory")
        .join("countdown-brain-fixture");
    assert!(
        path.is_file(),
        "the authored brain consumer fixture executable is not built at {}; build \
         it explicitly (`cargo build -p phoxal-countdown-brain-fixture`) before \
         running this suite",
        path.display()
    );
    path
}

struct TestBundle {
    _temporary_root: tempfile::TempDir,
    root: PathBuf,
}

/// Locates the compiled authored countdown fixture beside this package's
/// Runtime fixture binary.
///
/// The countdown provider is its own workspace package, so build it
/// explicitly first; the existence check cannot distinguish a current build
/// from a stale one: `cargo build --locked -p phoxal-countdown-fixture`.
fn countdown_binary() -> PathBuf {
    let path = std::path::Path::new(env!("CARGO_BIN_EXE_supervisor-test-runtime"))
        .parent()
        .expect("the Runtime fixture executable has a parent directory")
        .join("countdown-authoring-fixture");
    assert!(
        path.is_file(),
        "the authored countdown fixture executable is not built at {}; build \
         it explicitly (`cargo build -p phoxal-countdown-fixture`) before \
         running this suite",
        path.display()
    );
    path
}

fn build_bundle() -> TestBundle {
    let temporary_root = tempfile::tempdir().expect("temporary bundle root");
    let root = temporary_root.path().join("bundle");
    fs::create_dir_all(root.join("bin")).expect("bundle bin directory");

    let source = countdown_binary();
    let executable = root.join("bin/countdown");
    fs::copy(source, &executable).expect("copy the compiled countdown fixture");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("make the compiled countdown fixture executable");
    let consumer = consumer_binary();
    let consumer_executable = root.join("bin/brain");
    fs::copy(consumer, &consumer_executable).expect("copy the compiled brain consumer fixture");
    fs::set_permissions(&consumer_executable, fs::Permissions::from_mode(0o755))
        .expect("make the compiled brain consumer fixture executable");
    let document = serde_json::json!({
        "schema": "phoxal/robot/v0",
        "robot": {"id": "countdown-authoring", "model": null, "components": {}},
        "brain": null,
        "services": {
            "countdown": {
                "source": {"path": "../countdown"},
                "config": {}
            },
            "mission_countdown": {
                "source": {"path": "../countdown"},
                "config": {}
            }
        },
        "connections": {
            "brain.countdown_finished": "countdown.finished",
            "brain.countdown_status": "mission_countdown.status",
            "brain.start_countdown": "mission_countdown.start"
        }
    });
    let manifest = serde_json::json!({
        "schema": "phoxal/bundle/v0",
        "robot_id": "countdown-authoring",
        "root_package": {
            "id": "countdown-authoring",
            "name": "countdown-authoring",
            "source": "local"
        },
        "target": "host",
        "profile": "dev",
        "features": [],
        "executables": [{
            "role": "brain",
            "instance": "brain",
            "package_id": "countdown-authoring",
            "package": "countdown-authoring",
            "target": "phoxal-countdown-brain",
            "path": "bin/brain",
            "artifact": brain_artifact()
        }, {
            "role": "service",
            "instance": "countdown",
            "package_id": "countdown-authoring",
            "package": "countdown-authoring",
            "target": "phoxal-countdown-authoring",
            "path": "bin/countdown",
            "artifact": countdown_artifact()
        }, {
            "role": "service",
            "instance": "mission_countdown",
            "package_id": "countdown-authoring",
            "package": "countdown-authoring",
            "target": "phoxal-countdown-authoring",
            "path": "bin/countdown",
            "artifact": countdown_artifact()
        }],
        "components": []
    });
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("the source manifest serializes"),
    )
    .expect("write the source manifest");
    fs::write(
        root.join("robot.yaml"),
        serde_yaml::to_string(&document).expect("the compiled robot serializes"),
    )
    .expect("write the compiled robot");
    TestBundle {
        _temporary_root: temporary_root,
        root,
    }
}

/// The brain consumer fixture's expected record, shared with that binary's
/// own unit test (same consistency scope as the countdown record below).
fn brain_artifact() -> serde_json::Value {
    serde_json::json!({
        "runtime": serde_json::to_value(brain_expected_record::expected_runtime_record())
            .expect("the expected brain record serializes")
    })
}

/// The countdown fixture's complete expected runtime record, shared with
/// the binary's own unit test. Supervisor admission does NOT compare the
/// manifest's endpoint records against the linked binary record (it checks
/// the executable digest and timing/capability facts); what keeps this
/// bundle consistent with the binary is the fixture's unit test comparing
/// this same shared record against the macro-retained artifact record, so
/// the two cannot drift apart. That guarantee covers this fixture's
/// consistency only — it is not an admission mechanism.
fn countdown_artifact() -> serde_json::Value {
    serde_json::json!({
        "runtime": serde_json::to_value(expected_record::expected_runtime_record())
            .expect("the expected runtime record serializes")
    })
}
