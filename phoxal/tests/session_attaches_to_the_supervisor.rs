//! The two host profiles, meeting on a real execution.
//!
//! `phoxal::supervisor::host` is the whole supervisor and `phoxal::session` is
//! the whole attachment SDK; each is proven by its own unit tests, and neither
//! can see whether the other agrees with it. This test runs both: the
//! supervisor opens its embedded router over the fixture bundle, and a session
//! completes the frozen `supervisor/connect` bootstrap against it, reads the
//! manifest back, and closes.
//!
//! Everything it asserts is a fact one process learned from the other, which is
//! what makes it worth a whole execution: the framework train, the robot
//! identity out of `manifest.json`, and the baseline snapshot a late joiner
//! needs before it applies updates.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use phoxal::session::{ConnectOptions, Session};
use phoxal::supervisor::rendezvous::RuntimeRendezvous;
use phoxal::version::FrameworkVersion;

/// How long the supervisor is given to bind its socket. Binding is synchronous
/// inside `host::run`, so this is slack for the compile-time-sized fixture
/// staging around it rather than a readiness poll budget.
const STARTUP: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_attaches_to_a_live_supervisor_and_reads_the_running_robot() {
    let bundle = phoxal_fixture::staged_bundle();
    let root = bundle
        .path()
        .canonicalize()
        .expect("the staged bundle root resolves");
    // The staged bundle sits at `<release>/bundle`, and a bundle inside a
    // release is owned by the release - which is the rule a client applies to
    // find the same rendezvous the supervisor binds.
    let owning_root = root.parent().expect("a staged bundle has a release root");
    let socket = RuntimeRendezvous::for_root(owning_root).supervisor_socket();

    let bundle_root = root.clone();
    let supervisor = tokio::spawn(async move { phoxal::supervisor::host::run(&bundle_root).await });

    let endpoint = format!("unixsock-stream/{}", socket.display());
    let session = tokio::time::timeout(STARTUP, connect_when_bound(&endpoint, &supervisor))
        .await
        .expect("the supervisor binds its socket");

    let connected = session.connected().clone();
    assert_eq!(
        connected.framework,
        FrameworkVersion::CURRENT,
        "both halves of one train report the same version"
    );
    assert_eq!(
        connected.robot.as_str(),
        "rgbd-imu-diff-drive",
        "the identity comes from the manifest the supervisor is running"
    );

    let handle = session.handle();
    let manifest = handle.manifest().await.expect("the manifest answers");
    assert_eq!(
        manifest.into_robot().id().as_str(),
        connected.robot.as_str(),
        "`supervisor/info` hands back the same robot the bootstrap named"
    );
    let snapshot = handle
        .snapshot()
        .expect("a session installs the baseline snapshot before it returns");
    snapshot
        .validate()
        .expect("the baseline snapshot is internally consistent");

    session.close().await.expect("the session closes cleanly");
    supervisor.abort();
    let _ = supervisor.await;
}

/// Connect as soon as the supervisor is listening.
///
/// The supervisor is started in-process and this is the only wait in the test:
/// there is no readiness contract to poll, because a bound socket *is* the
/// readiness - `host::run` binds synchronously and fails the run otherwise.
async fn connect_when_bound(
    endpoint: &str,
    supervisor: &tokio::task::JoinHandle<phoxal::Result<()>>,
) -> Session {
    loop {
        assert!(
            !supervisor.is_finished(),
            "the supervisor exited before it was reachable at {endpoint}"
        );
        match Session::connect(&ConnectOptions::new(endpoint, "session-attach-test")).await {
            Ok(session) => return session,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}
