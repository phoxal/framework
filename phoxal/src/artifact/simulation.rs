//! Inert simulation evidence records.
//!
//! These are the records that flow off the simulator's stdout/stderr or
//! through the scenario control channel and are then consumed by the
//! supervisor, the SDK, and scenario authors. They retain observed paths,
//! artifact provenance and evidence without provisioning executables or
//! managing processes.
//!
//! Finite run reports and cleanup records are passive observations shared by
//! the simulator producer and developer-tool consumer. The simulator owns
//! native execution and process cleanup; developer tooling owns source
//! preparation and delegation. Neither policy moves into these records.

#![deny(unsafe_code)]

use std::{collections::BTreeMap, path::PathBuf};

use super::installation::SimulatorArtifactSummary;

use serde::{Deserialize, Serialize};

/// Bounded cleanup evidence for a local simulation process pair.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SimulationCleanup {
    /// Whether the supervisor was asked to stop.
    pub supervisor_stop_requested: bool,
    /// Whether the supervisor exited before the cleanup deadline.
    pub supervisor_exited: bool,
    /// Whether a forced kill was required.
    pub supervisor_killed: bool,
    /// Human-readable cleanup diagnostic, if cleanup was incomplete.
    pub error: Option<String>,
}

/// Terminal evidence retained by one local finite simulation run.
///
/// The default terminal is the shared consumer view. A producer may select its
/// richer terminal type, including a borrowed terminal for serialization,
/// preserving application-owned evidence without widening the SDK record.
/// Optional fields serialize explicitly as null when absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "schema")]
pub enum SimulationRunReport<Terminal = SimulatorTerminalEvidence> {
    /// The first simulation-run report generation.
    #[serde(rename = "phoxal/simulation-run/v0")]
    V0 {
        /// Canonical scene resource path.
        scene: PathBuf,
        /// Exact simulator artifact used.
        simulator: SimulatorArtifactSummary,
        /// Compiled simulation bundle path.
        bundle: PathBuf,
        /// Router namespace supplied to the run, or absent in failure evidence.
        scope: Option<String>,
        /// Supervisor identity supplied to the run, or absent in failure evidence.
        supervisor_id: Option<String>,
        /// Finite run identity supplied to the run, or absent in failure evidence.
        run_id: Option<String>,
        /// Whether the supervisor survived startup readiness.
        supervisor_ready: bool,
        /// Whether the simulator emitted the required provider-contract terminal
        /// evidence for the finite run.
        provider_contract_verified: bool,
        /// Simulator exit code, or none when it terminated by signal.
        simulator_exit_code: Option<i32>,
        /// Wall time spent inside the simulator process for this finite run.
        simulator_wall_time_ns: u64,
        /// Complete simulator standard output.
        simulator_stdout: String,
        /// Complete simulator standard error.
        simulator_stderr: String,
        /// Bounded supervisor cleanup evidence.
        cleanup: SimulationCleanup,
        /// Runtime-observed scenario evidence, when this was a scenario run.
        scenario: Option<ScenarioExecutionReport>,
        /// Parsed terminal evidence emitted by the native simulator.
        terminal: Option<Terminal>,
    },
}

impl<Terminal> SimulationRunReport<Terminal> {
    /// Whether the simulator exited successfully and cleanup completed.
    #[must_use]
    pub fn success(&self) -> bool {
        let Self::V0 {
            simulator_exit_code,
            cleanup,
            supervisor_ready,
            provider_contract_verified,
            ..
        } = self;
        *simulator_exit_code == Some(0)
            && cleanup.error.is_none()
            && cleanup.supervisor_exited
            && !cleanup.supervisor_killed
            && *supervisor_ready
            && *provider_contract_verified
    }
}

/// Aggregate scenario evidence for a single supervised run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "schema")]
pub enum ScenarioExecutionReport {
    /// The first scenario-execution report generation.
    #[serde(rename = "phoxal/scenario-execution/v0")]
    V0 {
        /// Fully qualified scenario name.
        scenario_name: String,
        /// Acknowledged scenario actions.
        steps: Vec<ScenarioStepEvidence>,
        /// Captured runtime records.
        captures: Vec<ScenarioCaptureEvidence>,
        /// Command replies keyed by authored step label.
        command_replies: BTreeMap<String, Vec<u8>>,
    },
}

/// One runtime-acknowledged scenario action.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScenarioStepEvidence {
    /// Authored step label.
    pub label: String,
    /// `setpoint`, `withdraw`, or `command`.
    pub kind: String,
    /// Boundary at which the action was published.
    pub production_boundary: u64,
    /// First boundary at which the action was eligible.
    pub eligible_boundary: u64,
}

/// One typed capture stream drained by the supervisor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScenarioCaptureEvidence {
    /// Authored capture name.
    pub name: String,
    /// `state`, `sample`, or `event`.
    pub kind: String,
    /// Ordered records retained according to the admitted capture policy.
    pub records: Vec<ScenarioObservationEvidence>,
    /// Whether a bounded best-effort capture discarded an earlier prefix.
    pub gap_before_first: bool,
    /// Whether required completeness was preserved through the final drain.
    pub complete: bool,
    /// Whether the supervisor completed the final drain for this capture.
    pub terminal: bool,
}

/// One observation with the provenance carried by its runtime envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScenarioObservationEvidence {
    /// Exact generated Protobuf payload bytes.
    pub payload: Vec<u8>,
    /// Original source identity, preserved through forwarding and replay.
    pub source: String,
    /// Original logical capture time in nanoseconds.
    pub capture_time_ns: u64,
    /// Monotonic source publication sequence.
    pub sequence: u64,
}

/// Native terminal evidence emitted by the simulator application.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "schema")]
pub enum SimulatorTerminalEvidence {
    /// The first simulator-terminal evidence generation.
    #[serde(rename = "phoxal/simulation-run/v0")]
    V0 {
        /// Whether the simulator's provider contract was independently verified.
        #[serde(rename = "provider_contract_verified")]
        provider_contract_verified: bool,
        /// `success` or `stopped`.
        outcome: String,
        /// Completed native transitions.
        completed_steps: u64,
        /// Requested native transitions.
        requested_steps: u64,
        /// Native quantum in nanoseconds.
        #[serde(default)]
        quantum_ns: u64,
        /// Supervisor execution identity.
        #[serde(default)]
        execution_id: String,
        /// Controlled timeline identity.
        #[serde(default)]
        timeline_id: String,
        /// Native root-body samples at 20 ms and terminal boundaries.
        #[serde(default)]
        native_body: Vec<NativeBodySample>,
    },
}

/// One native root-body sample in world coordinates.
///
/// This is the canonical wire type shared by simulator terminal records
/// and SDK scenario authors. Defined here and re-exported by
/// `phoxal::scenario::NativeBodySample`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct NativeBodySample {
    /// Native simulation boundary at which the sample was captured.
    pub boundary: u64,
    /// Physical root-body origin in world coordinates, in metres, not footprint or COM.
    pub position_m: [f64; 3],
    /// Body-to-world unit quaternion in `(w, x, y, z)` order.
    pub orientation_wxyz: [f64; 4],
    /// World-axis linear velocity at the physical body origin, in metres per second.
    pub linear_velocity_mps: [f64; 3],
    /// World-axis angular velocity in radians per second.
    pub angular_velocity_radps: [f64; 3],
}

#[cfg(test)]
mod tests {
    //! Round-trip tests ensure the canonical wire format survives any
    //! future internal refactor of these records.

    use super::*;

    fn run_report<Terminal>(terminal: Option<Terminal>) -> SimulationRunReport<Terminal> {
        SimulationRunReport::V0 {
            scene: PathBuf::from("/scene.xml"),
            simulator: SimulatorArtifactSummary {
                package: "phoxal-simulator".to_owned(),
                version: "0.2.4".to_owned(),
                binary: "phoxal-simulator".to_owned(),
                source: "installed-application".to_owned(),
                executable: PathBuf::from("/app/phoxal-simulator"),
            },
            bundle: PathBuf::from("/build"),
            scope: Some("robot/simulation".to_owned()),
            supervisor_id: Some("supervisor".to_owned()),
            run_id: Some("run-1".to_owned()),
            supervisor_ready: true,
            provider_contract_verified: true,
            simulator_exit_code: Some(0),
            simulator_wall_time_ns: 123,
            simulator_stdout: "raw stdout\n".to_owned(),
            simulator_stderr: "raw stderr\n".to_owned(),
            cleanup: SimulationCleanup {
                supervisor_stop_requested: true,
                supervisor_exited: true,
                supervisor_killed: false,
                error: None,
            },
            scenario: None,
            terminal,
        }
    }

    #[test]
    fn finite_run_report_preserves_exact_v0_fields_and_nulls() {
        let mut report: SimulationRunReport = run_report(None);
        let SimulationRunReport::V0 {
            simulator_exit_code,
            ..
        } = &mut report;
        *simulator_exit_code = None;
        let expected = serde_json::json!({
            "schema": "phoxal/simulation-run/v0",
            "scene": "/scene.xml",
            "simulator": {
                "package": "phoxal-simulator", "version": "0.2.4",
                "binary": "phoxal-simulator", "source": "installed-application",
                "executable": "/app/phoxal-simulator"
            },
            "bundle": "/build", "scope": "robot/simulation",
            "supervisor_id": "supervisor", "run_id": "run-1",
            "supervisor_ready": true, "provider_contract_verified": true,
            "simulator_exit_code": null, "simulator_wall_time_ns": 123,
            "simulator_stdout": "raw stdout\n", "simulator_stderr": "raw stderr\n",
            "cleanup": { "supervisor_stop_requested": true, "supervisor_exited": true,
                "supervisor_killed": false, "error": null },
            "scenario": null, "terminal": null
        });
        assert_eq!(serde_json::to_value(&report).expect("serializes"), expected);
        let decoded: SimulationRunReport = serde_json::from_value(expected).expect("deserializes");
        assert_eq!(decoded, report);
    }

    #[test]
    fn finite_run_report_preserves_each_absent_identity_as_explicit_null() {
        for field in ["scope", "supervisor_id", "run_id"] {
            let report: SimulationRunReport = run_report(None);
            let mut value = serde_json::to_value(&report).expect("serializes");
            value[field] = serde_json::Value::Null;
            let decoded: SimulationRunReport =
                serde_json::from_value(value.clone()).expect("absent identity deserializes");
            assert_eq!(serde_json::to_value(&decoded).expect("serializes"), value);
            assert!(
                decoded.success(),
                "identity validation belongs to the runtime"
            );
            let mut failed = value;
            failed["simulator_exit_code"] = serde_json::json!(1);
            let failed: SimulationRunReport =
                serde_json::from_value(failed).expect("failure decodes");
            assert!(!failed.success());
        }
        let mut value = serde_json::to_value(run_report::<SimulatorTerminalEvidence>(None))
            .expect("serializes");
        for field in ["scope", "supervisor_id", "run_id"] {
            value[field] = serde_json::Value::Null;
        }
        let decoded: SimulationRunReport = serde_json::from_value(value.clone()).expect("decodes");
        assert_eq!(serde_json::to_value(decoded).expect("serializes"), value);
    }

    #[test]
    fn finite_run_report_preserves_rich_borrowed_terminal_and_stdout() {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        struct RichTerminal {
            #[serde(flatten)]
            evidence: SimulatorTerminalEvidence,
            native_bindings: serde_json::Value,
            provenance: serde_json::Value,
            presentation: serde_json::Value,
            model_identity: String,
        }
        let terminal = RichTerminal {
            evidence: SimulatorTerminalEvidence::V0 {
                provider_contract_verified: true,
                outcome: "success".to_owned(),
                completed_steps: 2,
                requested_steps: 2,
                quantum_ns: 10_000_000,
                execution_id: "execution".to_owned(),
                timeline_id: "timeline".to_owned(),
                native_body: vec![NativeBodySample {
                    boundary: 2,
                    position_m: [1.0, 2.0, 3.0],
                    orientation_wxyz: [1.0, 0.0, 0.0, 0.0],
                    linear_velocity_mps: [0.5, 0.0, 0.0],
                    angular_velocity_radps: [0.0; 3],
                }],
            },
            native_bindings: serde_json::json!({"wheel": {"actuator": ["left", "right"]}}),
            provenance: serde_json::json!({"engine": {"sha256": "engine-hash"}}),
            presentation: serde_json::json!({"mode": "headless", "samples": [1, 2]}),
            model_identity: "model-hash".to_owned(),
        };
        let stdout = format!(
            "{}\n",
            serde_json::to_string_pretty(&terminal).expect("terminal serializes")
        );
        let mut borrowed = run_report(Some(&terminal));
        let SimulationRunReport::V0 {
            simulator_stdout,
            scenario,
            ..
        } = &mut borrowed;
        *simulator_stdout = stdout.clone();
        *scenario = Some(ScenarioExecutionReport::V0 {
            scenario_name: "drive".to_owned(),
            steps: Vec::new(),
            captures: Vec::new(),
            command_replies: BTreeMap::from([("reset".to_owned(), vec![0, 255])]),
        });
        let value = serde_json::to_value(&borrowed).expect("borrowed report serializes");
        assert_eq!(
            value["terminal"],
            serde_json::to_value(&terminal).expect("terminal serializes")
        );
        assert_eq!(value["simulator_stdout"], stdout);
        assert_eq!(
            value["terminal"]["native_bindings"]["wheel"]["actuator"],
            serde_json::json!(["left", "right"])
        );
        assert_eq!(
            value["terminal"]["provenance"]["engine"]["sha256"],
            "engine-hash"
        );
        assert_eq!(
            value["terminal"]["presentation"]["samples"],
            serde_json::json!([1, 2])
        );
        assert_eq!(value["terminal"]["model_identity"], "model-hash");
        let owned: SimulationRunReport<RichTerminal> =
            serde_json::from_value(value.clone()).expect("rich report deserializes");
        assert_eq!(
            serde_json::to_value(&owned).expect("owned report serializes"),
            value
        );
        let narrow: SimulationRunReport =
            serde_json::from_value(value).expect("consumer view deserializes");
        assert!(narrow.success());
        let SimulationRunReport::V0 {
            simulator_stdout,
            terminal: decoded,
            ..
        } = narrow;
        assert_eq!(simulator_stdout, stdout);
        assert_eq!(decoded, Some(terminal.evidence));
    }

    #[test]
    fn finite_run_success_preserves_exit_and_cleanup_predicate() {
        let success: SimulationRunReport = run_report(None);
        assert!(success.success());
        let mut report = success.clone();
        let SimulationRunReport::V0 { cleanup, .. } = &mut report;
        cleanup.supervisor_stop_requested = false;
        assert!(
            report.success(),
            "stop request is evidence, not a success prerequisite"
        );
        for code in [None, Some(1), Some(-1)] {
            let mut report = success.clone();
            let SimulationRunReport::V0 {
                simulator_exit_code,
                ..
            } = &mut report;
            *simulator_exit_code = code;
            assert!(!report.success());
        }
        for failure in 0..5 {
            let mut report = success.clone();
            let SimulationRunReport::V0 {
                cleanup,
                supervisor_ready,
                provider_contract_verified,
                ..
            } = &mut report;
            match failure {
                0 => cleanup.supervisor_exited = false,
                1 => cleanup.supervisor_killed = true,
                2 => cleanup.error = Some(String::new()),
                3 => *supervisor_ready = false,
                4 => *provider_contract_verified = false,
                _ => unreachable!(),
            }
            assert!(!report.success());
        }
    }

    #[test]
    fn native_body_sample_round_trips() {
        let sample = NativeBodySample {
            boundary: 7,
            position_m: [1.0, 2.0, 3.0],
            orientation_wxyz: [1.0, 0.0, 0.0, 0.0],
            linear_velocity_mps: [0.0, 0.0, 0.0],
            angular_velocity_radps: [0.0, 0.0, 0.0],
        };
        let json = serde_json::to_string(&sample).expect("serializes");
        let decoded: NativeBodySample = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, sample);
    }

    #[test]
    fn terminal_evidence_round_trips_with_default_native_body() {
        let evidence = SimulatorTerminalEvidence::V0 {
            provider_contract_verified: true,
            outcome: "success".to_owned(),
            completed_steps: 100,
            requested_steps: 100,
            quantum_ns: 0,
            execution_id: String::new(),
            timeline_id: String::new(),
            native_body: Vec::new(),
        };
        let json = serde_json::to_string(&evidence).expect("serializes");
        let decoded: SimulatorTerminalEvidence = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, evidence);
    }

    #[test]
    fn scenario_execution_report_round_trips() {
        let report = ScenarioExecutionReport::V0 {
            scenario_name: "forward_turn_stop".to_owned(),
            steps: vec![ScenarioStepEvidence {
                label: "drive".to_owned(),
                kind: "setpoint".to_owned(),
                production_boundary: 4,
                eligible_boundary: 0,
            }],
            captures: vec![ScenarioCaptureEvidence {
                name: "pose".to_owned(),
                kind: "state".to_owned(),
                records: vec![ScenarioObservationEvidence {
                    payload: b"\x00\x01".to_vec(),
                    source: "world.pose".to_owned(),
                    capture_time_ns: 8_000_000,
                    sequence: 4,
                }],
                gap_before_first: false,
                complete: true,
                terminal: true,
            }],
            command_replies: BTreeMap::from([("reset".to_owned(), b"\x02".to_vec())]),
        };
        let json = serde_json::to_string(&report).expect("serializes");
        let decoded: ScenarioExecutionReport = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, report);
    }
}
