//! Owned process group for supervisor executable acceptance.
//!
//! Each test target includes this module separately, so a helper is dead
//! code in every target that does not call it; the allow keeps the shared
//! module compilable for every consumer.

#![allow(dead_code)]

use std::{path::Path, path::PathBuf, time::Duration};

/// Locates the compiled supervisor executable beside this package's Runtime
/// fixture binary.
///
/// The supervisor is its own workspace package and this package's `cargo
/// test` invocation builds only its test harness, not the normal supervisor
/// executable. Build it explicitly first; the existence check below cannot
/// distinguish a current build from a stale one:
///
/// ```sh
/// cargo build --locked -p phoxal-supervisor --bin phoxal-supervisor
/// cargo test --locked -p phoxal-supervisor-runtime-fixture -p phoxal-supervisor --features phoxal-supervisor-runtime-fixture/test-fixtures
/// ```
pub fn supervisor_binary() -> PathBuf {
    let path = Path::new(env!("CARGO_BIN_EXE_supervisor-test-runtime"))
        .parent()
        .expect("the Runtime fixture executable has a parent directory")
        .join("phoxal-supervisor");
    assert!(
        path.is_file(),
        "the supervisor executable is not built at {}; build it explicitly \
         (`cargo build -p phoxal-supervisor --bin phoxal-supervisor`) before \
         running these suites",
        path.display()
    );
    path
}

pub fn reference_runtime_artifact() -> serde_json::Value {
    serde_json::json!({
        "runtime": {
            "schema": "phoxal/artifact/v0",
            "record": "runtime",
            "period_ms": 20,
            "timeout_ms": 100,
            "init_timeout_ms": 1000,
            "config_schema": {"type": "null"},
            "inputs": [{
                "name": "read",
                "role": "call_ingress",
                "max_items": 8,
                "max_bytes": 4096,
                "port": "read",
                "signature": {
                    "endpoint": "read",
                    "service": "example.inspection.v1.Read",
                    "method": "read",
                    "shape": "call",
                    "request": "example.inspection.v1.InspectionReadRequest",
                    "response": "example.inspection.v1.InspectionReadResponse",
                    "retained_latest": false,
                    "lease_valid_for_ms": null
                }
            }, {
                "name": "calibrate",
                "role": "call_ingress",
                "max_items": 4,
                "max_bytes": 1024,
                "port": "calibrate",
                "signature": {
                    "endpoint": "calibrate",
                    "service": "example.inspection.v1.Calibrate",
                    "method": "calibrate",
                    "shape": "call",
                    "request": "google.protobuf.Empty",
                    "response": "google.protobuf.Empty",
                    "retained_latest": false,
                    "lease_valid_for_ms": null
                }
            }],
            "transient_outputs": [{
                "name": "read_replies",
                "role": "reply",
                "input": "read",
                "max_items": 8,
                "max_bytes": 4096
            }, {
                "name": "calibrate_replies",
                "role": "reply",
                "input": "calibrate",
                "max_items": 4,
                "max_bytes": 1024
            }],
            "service_outputs": [
                {
                    "name": "status",
                    "role": "method",
                    "port": "status",
                    "max_items": 1,
                    "max_bytes": 4096,
                    "bootstrap": true,
                    "on_change": false,
                    "signature": {
                        "endpoint": "status",
                        "service": "example.inspection.v1.InspectionState",
                        "method": "status",
                        "shape": "observation",
                        "request": "google.protobuf.Empty",
                        "response": "example.inspection.v1.InspectionState",
                        "retained_latest": true,
                        "lease_valid_for_ms": null
                    }
                },
                {
                    "name": "encoder",
                    "role": "method",
                    "port": "encoder",
                    "max_items": 16,
                    "max_bytes": 8192,
                    "bootstrap": false,
                    "on_change": false,
                    "signature": {
                        "endpoint": "encoder",
                        "service": "phoxal.robotics.v1.EncoderSample",
                        "method": "encoder",
                        "shape": "observation",
                        "request": "google.protobuf.Empty",
                        "response": "phoxal.robotics.v1.EncoderSample",
                        "retained_latest": false,
                        "lease_valid_for_ms": null
                    }
                }
            ],
            "descriptors": []
        }
    })
}

pub struct SupervisorProcess {
    child: tokio::process::Child,
    group: i32,
}

impl SupervisorProcess {
    pub fn launch(bundle: &Path, id: &str) -> Self {
        let mut command = tokio::process::Command::new(supervisor_binary());
        command
            .arg(bundle)
            .args(["--scope", "local", "--supervisor-id", id]);
        command.process_group(0).kill_on_drop(true);
        let child = command.spawn().expect("launch supervisor executable");
        let group = child.id().expect("live supervisor PID") as i32;
        Self { child, group }
    }

    pub fn is_finished(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("query supervisor exit")
            .is_some()
    }

    pub async fn shutdown(&mut self) {
        // SAFETY: this positive PID belongs to the live child retained by this guard.
        assert_eq!(unsafe { libc::kill(self.group, libc::SIGTERM) }, 0);
        let status = tokio::time::timeout(Duration::from_secs(15), self.child.wait())
            .await
            .expect("bounded supervisor shutdown")
            .expect("reap supervisor");
        assert!(status.success(), "supervisor shutdown failed: {status}");
    }
}

impl Drop for SupervisorProcess {
    fn drop(&mut self) {
        // SAFETY: the child was started in its own process group. Kill only that group,
        // including runtime children, when an assertion unwinds before normal shutdown.
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
    }
}
