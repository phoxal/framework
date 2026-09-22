//! Inert simulation evidence records.
//!
//! These are the records that flow off the simulator's stdout/stderr or
//! through the scenario control channel and are then consumed by the
//! supervisor, the SDK, and scenario authors. They contain no
//! process-orchestration state and no provisioned executable paths; they
//! describe observed evidence only.
//!
//! Process orchestration (`SimulationRunOptions`, `SimulationRunReport`,
//! `SimulationCleanup`, simulator provisioning, child-process management)
//! stays with the simulator integration in `cargo-phoxal`.

#![deny(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
    /// Root-body position in metres.
    pub position_m: [f64; 3],
    /// Root-body orientation as a unit quaternion `(w, x, y, z)`.
    pub orientation_wxyz: [f64; 4],
    /// Root-body linear velocity in metres per second.
    pub linear_velocity_mps: [f64; 3],
    /// Root-body angular velocity in radians per second.
    pub angular_velocity_radps: [f64; 3],
}

#[cfg(test)]
mod tests {
    //! Round-trip tests ensure the canonical wire format survives any
    //! future internal refactor of these records.

    use super::*;

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
