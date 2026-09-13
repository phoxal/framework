//! The two host profiles, meeting on a real execution.
//!
//! `phoxal::supervisor::host` is the whole supervisor and `phoxal::session` is
//! the whole attachment SDK; each is proven by its own unit tests, and neither
//! can see whether the other agrees with it. This test runs both: the
//! supervisor opens its embedded router over the fixture bundle, and a session
//! completes the frozen `supervisor/connect` bootstrap against it and closes.
//!
//! Everything it asserts is a fact one process learned from the other, which is
//! what makes it worth a whole execution: the framework train and the
//! supervisor identity returned by the public session bootstrap.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use phoxal::session::{Connection, ConnectionConfig, connect};
use phoxal::supervisor::rendezvous::RuntimeRendezvous;
use phoxal::version::FrameworkVersion;

/// How long the supervisor is given to bind its socket. Binding is synchronous
/// inside `host::run`, so this is slack for the compile-time-sized fixture
/// staging around it rather than a readiness poll budget.
const STARTUP: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_attaches_to_a_live_supervisor() {
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
    let supervisor = tokio::spawn(async move {
        phoxal::supervisor::host::run(
            &bundle_root,
            phoxal::communication::DeploymentTarget::new("local", "local")
                .expect("valid isolated local deployment identity"),
        )
        .await
    });

    let endpoint = format!("unixsock-stream/{}", socket.display());
    let connection = tokio::time::timeout(STARTUP, connect_when_bound(&endpoint, &supervisor))
        .await
        .expect("the supervisor binds its socket");
    let supervisor_session = connection
        .supervisor("local")
        .await
        .expect("the supervisor accepts the public session");
    let info = supervisor_session
        .info()
        .await
        .expect("the supervisor info answers");
    assert_eq!(
        info.framework_version,
        FrameworkVersion::CURRENT.to_string(),
        "both halves of one train report the same version"
    );
    assert_eq!(
        info.supervisor_version,
        env!("CARGO_PKG_VERSION"),
        "the supervisor reports its package version"
    );
    supervisor_session
        .close()
        .await
        .expect("the supervisor session closes cleanly");
    connection
        .close()
        .await
        .expect("the connection closes cleanly");
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
) -> Connection {
    loop {
        assert!(
            !supervisor.is_finished(),
            "the supervisor exited before it was reachable at {endpoint}"
        );
        let config = ConnectionConfig::new(endpoint, "local", "session-attach-test")
            .expect("the session config is valid");
        match connect(config).await {
            Ok(connection) => return connection,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}
