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

/// Evidence schema identifier for the simulator's terminal output.
pub const SIMULATION_RUN_SCHEMA: &str = "phoxal/simulation-run/v0";

/// Aggregate scenario evidence for a single supervised run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScenarioExecutionReport {
    /// Evidence schema identifier.
    pub schema: String,
    /// Fully qualified scenario name.
    pub scenario_name: String,
    /// Acknowledged scenario actions.
    pub steps: Vec<ScenarioStepEvidence>,
    /// Captured runtime records.
    pub captures: Vec<ScenarioCaptureEvidence>,
    /// Command replies keyed by authored step label.
    pub command_replies: BTreeMap<String, Vec<u8>>,
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
    /// Boundary of the most recently observed payload.
    pub boundary: u64,
    /// Ordered payload bytes. State captures retain only the latest value.
    pub payloads: Vec<Vec<u8>>,
}

/// Native terminal evidence emitted by the simulator application.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SimulatorTerminalEvidence {
    /// Evidence schema.
    pub schema: String,
    #[serde(rename = "provider_contract_verified")]
    /// Whether the simulator's provider contract was independently verified.
    pub provider_contract_verified: bool,
    /// `success` or `stopped`.
    pub outcome: String,
    /// Completed native transitions.
    pub completed_steps: u64,
    /// Requested native transitions.
    pub requested_steps: u64,
    /// Native quantum in nanoseconds.
    #[serde(default)]
    pub quantum_ns: u64,
    /// Supervisor execution identity.
    #[serde(default)]
    pub execution_id: String,
    /// Controlled timeline identity.
    #[serde(default)]
    pub timeline_id: String,
    /// Native root-body samples at 20 ms and terminal boundaries.
    #[serde(default)]
    pub native_body: Vec<NativeBodySample>,
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
        let evidence = SimulatorTerminalEvidence {
            schema: SIMULATION_RUN_SCHEMA.to_owned(),
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
        let report = ScenarioExecutionReport {
            schema: "phoxal/scenario-evidence/v0".to_owned(),
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
                boundary: 4,
                payloads: vec![b"\x00\x01".to_vec()],
            }],
            command_replies: BTreeMap::from([("reset".to_owned(), b"\x02".to_vec())]),
        };
        let json = serde_json::to_string(&report).expect("serializes");
        let decoded: ScenarioExecutionReport = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, report);
    }
}
