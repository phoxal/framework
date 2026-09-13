//! The two host profiles, meeting on a real execution.
//!
//! `phoxal::supervisor::host` is the whole supervisor and `phoxal::session` is
//! the whole attachment SDK; each is proven by its own unit tests, and neither
//! can see whether the other agrees with it. This test runs both: the
//! supervisor opens its embedded router over a compiled Runtime bundle, and a
//! session completes the frozen `supervisor/connect` bootstrap against it and
//! closes.
//!
//! Everything it asserts is a fact one process learned from the other, which is
//! what makes it worth a whole execution: the framework train and the
//! supervisor identity returned by the public session bootstrap.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use phoxal::session::{Connection, ConnectionConfig, connect};
use phoxal::supervisor::rendezvous::RuntimeRendezvous;
use sha2::{Digest, Sha256};

/// How long the supervisor is given to bind its socket. Binding is synchronous
/// inside `host::run`, so this is slack for the compile-time-sized fixture
/// staging around it rather than a readiness poll budget.
const STARTUP: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_attaches_to_a_live_supervisor() {
    let bundle = build_bundle();
    let root = bundle
        .root
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
        env!("CARGO_PKG_VERSION"),
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

struct TestBundle {
    _temporary_root: tempfile::TempDir,
    root: PathBuf,
}

fn build_bundle() -> TestBundle {
    let temporary_root = tempfile::tempdir().expect("temporary bundle root");
    let root = temporary_root.path().join("bundle");
    fs::create_dir_all(root.join("bin")).expect("bundle bin directory");

    let source = Path::new(env!("CARGO_BIN_EXE_phoxal-runtime-reference"));
    let executable = root.join("bin/brain");
    fs::copy(source, &executable).expect("copy compiled Runtime fixture");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("make compiled Runtime fixture executable");
    let bytes = fs::read(&executable).expect("read copied Runtime fixture");
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let artifact = phoxal_project::artifact::inspect_file(&executable)
        .expect("inspect compiled Runtime fixture")
        .summary();
    let manifest = serde_json::json!({
        "schema": "phoxal/bundle/v0",
        "robot_id": "session-attachment",
        "document": {
            "schema": "phoxal/robot/v0",
            "robot": {
                "id": "session-attachment",
                "model": null,
                "kinematic": null,
                "motion_limits": null,
                "components": {}
            },
            "brain": null,
            "services": {},
            "connections": {}
        },
        "root_package": {
            "id": "session-attachment",
            "name": "session-attachment",
            "source": "local"
        },
        "target": "host",
        "profile": "dev",
        "features": [],
        "executables": [{
            "role": "brain",
            "instance": "brain",
            "package_id": "session-attachment",
            "package": "session-attachment",
            "target": "phoxal-runtime-reference",
            "path": "bin/brain",
            "bytes": bytes.len(),
            "sha256": sha256,
            "artifact": artifact
        }],
        "components": []
    });
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("source manifest serializes"),
    )
    .expect("write source manifest");
    TestBundle {
        _temporary_root: temporary_root,
        root,
    }
}
