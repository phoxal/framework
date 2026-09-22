//! Acceptance through the actual supervisor executable and runtime child.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use phoxal::communication::session::SupervisorState;
use phoxal::session::{Connection, ConnectionConfig, Supervisor, connect};
use sha2::{Digest, Sha256};

const STARTUP: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compiled_runtime_crosses_supervisor_and_zenoh_before_termination() {
    let bundle = build_bundle();
    let root = bundle
        .root
        .canonicalize()
        .expect("bundle root canonicalizes");
    let socket = root
        .parent()
        .expect("bundle has an owning release root")
        .join(".phoxal/run/supervisor.sock");
    let endpoint = format!("unixsock-stream/{}", socket.display());

    let mut supervisor = support::SupervisorProcess::launch(&root, "runtime-e2e");

    let connection =
        tokio::time::timeout(STARTUP, connect_when_bound(&endpoint, &mut supervisor)).await;
    let connection = connection.expect("the supervisor binds its public session endpoint");
    let supervisor_session = connection
        .supervisor("runtime-e2e")
        .await
        .expect("the supervisor accepts the public session");
    tokio::time::timeout(STARTUP, wait_until_ready(&supervisor_session))
        .await
        .expect("runtime reaches Ready before the startup deadline")
        .expect("runtime remains healthy while reaching Ready");
    tokio::time::timeout(STARTUP, wait_for_step(&root))
        .await
        .expect("runtime executes a bounded step")
        .expect("runtime step marker is readable");

    supervisor_session
        .close()
        .await
        .expect("the supervisor session closes cleanly");
    connection
        .close()
        .await
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
        let config = ConnectionConfig::new(endpoint, "local", "runtime-e2e")
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
                anyhow::bail!("supervisor entered Failed before Runtime Ready")
            }
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

async fn wait_for_step(root: &Path) -> phoxal::Result<()> {
    loop {
        if fs::read(root.join("reference-runtime.marker")).is_ok_and(|value| value == b"stepped") {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
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

    let source = Path::new(env!("CARGO_BIN_EXE_supervisor-test-runtime"));
    let executable = root.join("bin/brain");
    fs::copy(source, &executable).expect("copy compiled Runtime fixture");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("make compiled Runtime fixture executable");
    let bytes = fs::read(&executable).expect("read copied Runtime fixture");
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let manifest = serde_json::json!({
        "schema": "phoxal/bundle/v0",
        "robot_id": "runtime-reference",
        "document": {
            "schema": "phoxal/robot/v0",
            "robot": {
                "id": "runtime-reference",
                "model": null,
                "components": {}
            },
            "brain": null,
            "services": {},
            "connections": {}
        },
        "root_package": {
            "id": "runtime-reference",
            "name": "runtime-reference",
            "source": "local"
        },
        "target": "host",
        "profile": "dev",
        "features": [],
        "executables": [{
            "role": "brain",
            "instance": "brain",
            "package_id": "runtime-reference",
            "package": "runtime-reference",
            "target": "phoxal-runtime-reference",
            "path": "bin/brain",
            "bytes": bytes.len(),
            "sha256": sha256,
            "artifact": support::reference_runtime_artifact()
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
