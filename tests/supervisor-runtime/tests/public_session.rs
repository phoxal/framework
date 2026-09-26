//! Acceptance through the actual supervisor executable and runtime child.

#![allow(clippy::expect_used, clippy::unwrap_used)]
#![recursion_limit = "256"]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use phoxal::session::{CallOutcome, Connection, ConnectionConfig, ObservationItem, connect};
phoxal::api!();
use crate::api::types::example::inspection::v1::InspectionReadRequest;

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
    let socket = owning_root.join(".phoxal/run/supervisor.sock");

    let mut supervisor = support::SupervisorProcess::launch(&root, "local");

    let endpoint = format!("unixsock-stream/{}", socket.display());
    let connection = tokio::time::timeout(STARTUP, connect_when_bound(&endpoint, &mut supervisor))
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
    assert!(
        !info.framework_version.is_empty() && !info.supervisor_version.is_empty(),
        "the supervisor reports its framework and package versions"
    );
    tokio::time::timeout(STARTUP, async {
        loop {
            assert!(
                !supervisor.is_finished(),
                "supervisor exited during admission"
            );
            let status = supervisor_session
                .management()
                .status()
                .await
                .expect("public status");
            match phoxal::communication::session::SupervisorState::try_from(status.state).unwrap() {
                phoxal::communication::session::SupervisorState::Ready => break,
                phoxal::communication::session::SupervisorState::Failed => {
                    panic!("admission failed: {:?}", status.detail)
                }
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await
    .expect("runtime admission reaches Ready");
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
        .expect("select admitted execution");
    let brain = execution
        .service("brain")
        .await
        .expect("select brain service instance");

    let status = brain
        .method(crate::api::service_methods::u0::STATUS)
        .await
        .expect("bind generated observation method");
    let mut observations = status.observe().await.expect("start observation");
    tokio::time::timeout(STARTUP, async {
        loop {
            let observation = observations
                .recv()
                .await
                .expect("observation stream remains open")
                .expect("observation decodes");
            match observation {
                ObservationItem::InitialAbsent { .. } => continue,
                ObservationItem::Value { value, .. } => {
                    assert!(value.active);
                    break;
                }
                unexpected => panic!("unexpected observation record: {unexpected:?}"),
            }
        }
    })
    .await
    .expect("the generated runtime observation arrives");

    let read = brain
        .method(crate::api::service_methods::u0::READ)
        .await
        .expect("bind generated call method");
    let outcome = read
        .call(
            InspectionReadRequest {
                key: "status".to_owned(),
            },
            STARTUP,
        )
        .await
        .expect("call transport completes");
    assert!(
        matches!(
            outcome,
            CallOutcome::Received(response)
                if response.state.is_some_and(|state| state.active && state.count > 0)
        ),
        "the generated call completes through the real runtime"
    );
    supervisor_session
        .close()
        .await
        .expect("the supervisor session closes cleanly");
    connection
        .close()
        .await
        .expect("the connection closes cleanly");
    supervisor.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires independently installed Motion and a prepared robot consumer"]
async fn installed_motion_contract_crosses_the_real_runner_and_supervisor() {
    let binary = std::env::var_os("PHOXAL_PACKAGED_MOTION_BINARY")
        .map(PathBuf::from)
        .expect("set PHOXAL_PACKAGED_MOTION_BINARY to the installed Motion executable");
    let client = std::env::var_os("PHOXAL_PACKAGED_MOTION_CONSUMER")
        .expect("set PHOXAL_PACKAGED_MOTION_CONSUMER to the prepared robot executable");
    let bundle = build_motion_bundle(&binary);
    let root = bundle.root.canonicalize().expect("bundle root resolves");
    let socket = root
        .parent()
        .expect("bundle has an owning release root")
        .join(".phoxal/run/supervisor.sock");
    let endpoint = format!("unixsock-stream/{}", socket.display());
    let mut supervisor = support::SupervisorProcess::launch(&root, "motion-contract");
    let connection = tokio::time::timeout(STARTUP, connect_when_bound(&endpoint, &mut supervisor))
        .await
        .expect("the supervisor binds its socket");
    let supervisor_session = connection
        .supervisor("motion-contract")
        .await
        .expect("the supervisor accepts the session");
    tokio::time::timeout(STARTUP, async {
        loop {
            let status = supervisor_session
                .management()
                .status()
                .await
                .expect("status");
            match phoxal::communication::session::SupervisorState::try_from(status.state)
                .expect("known state")
            {
                phoxal::communication::session::SupervisorState::Ready => break,
                phoxal::communication::session::SupervisorState::Failed => {
                    panic!(
                        "Motion contract runtime admission failed: {:?}",
                        status.detail
                    )
                }
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await
    .expect("Motion runtime reaches Ready");
    supervisor_session.close().await.expect("session closes");
    connection.close().await.expect("connection closes");
    let output = tokio::process::Command::new(client)
        .arg(&endpoint)
        .output()
        .await
        .expect("start separately prepared robot consumer");
    assert!(
        output.status.success(),
        "separate consumer failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    supervisor.shutdown().await;
}

/// Connect as soon as the supervisor is listening.
///
/// The supervisor is started in-process and this is the only wait in the test:
/// there is no readiness contract to poll, because a bound socket *is* the
/// readiness - `host::run` binds synchronously and fails the run otherwise.
async fn connect_when_bound(
    endpoint: &str,
    supervisor: &mut support::SupervisorProcess,
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

    let source = PathBuf::from(env!("CARGO_BIN_EXE_supervisor-test-runtime"));
    let executable = root.join("bin/brain");
    fs::copy(source, &executable).expect("copy compiled Runtime fixture");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("make compiled Runtime fixture executable");
    let document = serde_json::json!({
        "schema": "phoxal/robot/v0",
        "robot": {"id": "session-attachment", "model": null, "components": {}},
        "brain": null,
        "services": {},
        "connections": {}
    });
    let manifest = serde_json::json!({
        "schema": "phoxal/bundle/v0",
        "robot_id": "session-attachment",
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
            "artifact": support::reference_runtime_artifact()
        }],
        "components": []
    });
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("source manifest serializes"),
    )
    .expect("write source manifest");
    fs::write(
        root.join("robot.yaml"),
        serde_yaml::to_string(&document).expect("compiled robot serializes"),
    )
    .expect("write compiled robot");
    TestBundle {
        _temporary_root: temporary_root,
        root,
    }
}

fn build_motion_bundle(packaged: &Path) -> TestBundle {
    let temporary_root = tempfile::tempdir().expect("temporary bundle root");
    let root = temporary_root.path().join("bundle");
    fs::create_dir_all(root.join("bin")).expect("bundle bin directory");

    let executable = root.join("bin/motion");
    fs::copy(packaged, &executable).expect("copy installed Motion executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("make Motion contract Runtime executable");
    let bytes = fs::read(&executable).expect("read copied Runtime fixture");
    let brain_source = PathBuf::from(env!("CARGO_BIN_EXE_supervisor-test-runtime"));
    let brain_executable = root.join("bin/brain");
    fs::copy(brain_source, &brain_executable).expect("copy compiled brain Runtime");
    fs::set_permissions(&brain_executable, fs::Permissions::from_mode(0o755))
        .expect("make brain Runtime executable");
    let brain_bytes = fs::read(&brain_executable).expect("read copied brain Runtime");
    let artifact = packaged_artifact(&bytes);
    let config = serde_json::json!({
        "max_linear_mps": 1.0,
        "max_angular_radps": 1.0,
        "wheel_radius_m": 0.1,
        "wheel_base_m": 0.4,
        "left_wheels": [{"actuator_id": "left"}],
        "right_wheels": [{"actuator_id": "right"}]
    });
    let document = serde_json::json!({
        "schema": "phoxal/robot/v0",
        "robot": {"id": "motion-contract-qualification", "model": null, "components": {}},
        "brain": null,
        "services": {"motion": {
            "source": {"package": {"name": "phoxal-service-motion", "version": "0.0.0-dev.3"}},
            "config": config
        }},
        "connections": {
            "motion.safety": "brain.constraints",
            "motion.measurements": "brain.odometry"
        }
    });
    let manifest = serde_json::json!({
        "schema": "phoxal/bundle/v0",
        "robot_id": "motion-contract-qualification",
        "root_package": {"id": "motion-contract-qualification", "name": "motion-contract-qualification", "source": "local"},
        "target": "host",
        "profile": "dev",
        "features": [],
        "executables": [
            {
                "role": "brain",
                "instance": "brain",
                "package_id": "motion-contract-qualification",
                "package": "motion-contract-qualification",
                "target": "supervisor-test-runtime",
                "path": "bin/brain",
                "artifact": packaged_artifact(&brain_bytes)
            },
            {
                "role": "service",
                "instance": "motion",
                "package_id": "phoxal-service-motion",
                "package": "phoxal-service-motion",
                "target": "phoxal-service-motion",
                "path": "bin/motion",
                "artifact": artifact
            }
        ],
        "components": []
    });
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("manifest serializes"),
    )
    .expect("write manifest");
    fs::write(
        root.join("robot.yaml"),
        serde_yaml::to_string(&document).expect("compiled robot serializes"),
    )
    .expect("write compiled robot");
    TestBundle {
        _temporary_root: temporary_root,
        root,
    }
}

fn packaged_artifact(bytes: &[u8]) -> serde_json::Value {
    let magic = b"PHXART0\n";
    let offsets = bytes
        .windows(magic.len())
        .enumerate()
        .filter_map(|(offset, window)| (window == magic).then_some(offset))
        .collect::<Vec<_>>();
    assert_eq!(offsets.len(), 1, "installed Motion has one runtime record");
    let start = offsets[0] + magic.len();
    let length = u32::from_le_bytes(
        bytes[start..start + 4]
            .try_into()
            .expect("runtime record length"),
    ) as usize;
    let runtime =
        serde_json::from_slice::<serde_json::Value>(&bytes[start + 4..start + 4 + length])
            .expect("installed Motion runtime record decodes");
    let descriptor_magic = &phoxal::contract::DESCRIPTOR_FRAME_MAGIC;
    let descriptors = bytes
        .windows(descriptor_magic.len())
        .enumerate()
        .filter_map(|(offset, window)| (window == descriptor_magic).then_some(offset))
        .map(|offset| {
            let start = offset + descriptor_magic.len();
            let length = u64::from_le_bytes(
                bytes[start..start + 8]
                    .try_into()
                    .expect("descriptor frame length"),
            ) as usize;
            let raw = &bytes[start + 8..start + 8 + length];
            let pool = prost_reflect::DescriptorPool::decode(raw)
                .expect("installed binary descriptor closure decodes");
            serde_json::json!({
                "sha256": format!("{:x}", Sha256::digest(raw)),
                "bytes": raw.len(),
                "files": pool.files().map(|file| file.name().to_owned()).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    assert!(
        !descriptors.is_empty(),
        "installed binary retains API descriptors"
    );
    serde_json::json!({"runtime": runtime, "descriptors": descriptors})
}
