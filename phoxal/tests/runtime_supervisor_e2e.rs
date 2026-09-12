//! A compiled Runtime crossing the source-bundle supervisor and Zenoh boundary.
//!
//! The fixture binary is copied into a source `phoxal/bundle/v0` manifest, so
//! the supervisor has to admit its digest, launch the exact selected path,
//! and observe the child's execution-scoped Ready liveliness token through the
//! bus-owned transport test helper.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

    let supervisor_root = root.clone();
    let supervisor = tokio::spawn(async move {
        phoxal::supervisor::host::run(
            &supervisor_root,
            phoxal::communication::DeploymentTarget::new("local", "runtime-e2e")
                .expect("valid deployment target"),
        )
        .await
    });

    let ready_result = tokio::time::timeout(
        STARTUP,
        phoxal::__bus_test_support::wait_for_participant_ready(&endpoint, "brain"),
    )
    .await;
    match ready_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("runtime Ready observer remains live: {error:#}"),
        Err(_) => {
            if supervisor.is_finished() {
                eprintln!("supervisor result: {:?}", supervisor.await);
            }
            panic!("runtime Ready arrives before the startup deadline");
        }
    }
    tokio::time::timeout(STARTUP, wait_for_step(&root))
        .await
        .expect("runtime executes a bounded step")
        .expect("runtime step marker is readable");

    supervisor.abort();
    let _ = supervisor.await;
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

    let source = Path::new(env!("CARGO_BIN_EXE_phoxal-runtime-reference"));
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
                "kinematic": null,
                "motion_limits": null,
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
            "artifact": {}
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
