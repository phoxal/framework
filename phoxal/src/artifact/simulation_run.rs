//! Simulation-only experiment specification.
//!
//! A simulation run specification is separate from an immutable robot bundle.
//! It binds one finite experiment to the exact bundle manifest, model, and
//! execution bounds selected by the tool. Hardware launch paths reject the
//! presence of this artifact before starting any child process.

#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};

use super::MethodSignature;

/// Serialized specification for one finite simulation experiment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum SimulationRunSpecification {
    /// First simulation run specification generation.
    #[serde(rename = "phoxal/simulation-run-specification/v0")]
    V0 {
        /// Exact immutable robot bundle selected for this experiment.
        bundle: SimulationBundleReference,
        /// Exact closed model selected by the simulator probe.
        model: SimulationModelReference,
        /// Exact independently managed simulator application selection.
        simulator: SimulationApplicationReference,
        /// Canonical finite scenario program and its recorded identity.
        program: SimulationProgram,
        /// Observation and native evidence requested by the experiment.
        captures: Vec<SimulationCaptureRequirement>,
        /// Finite controlled-execution bounds.
        execution: SimulationExecutionBounds,
    },
}

impl SimulationRunSpecification {
    /// Return the referenced bundle manifest digest.
    #[must_use]
    pub fn bundle_manifest_sha256(&self) -> &str {
        match self {
            Self::V0 { bundle, .. } => &bundle.manifest_sha256,
        }
    }

    /// Return the robot identity recorded in the selected bundle.
    #[must_use]
    pub fn robot_id(&self) -> &str {
        match self {
            Self::V0 { bundle, .. } => &bundle.robot_id,
        }
    }

    /// Return the finite program record.
    #[must_use]
    pub fn program(&self) -> &SimulationProgram {
        match self {
            Self::V0 { program, .. } => program,
        }
    }

    /// Return the controlled-execution bounds.
    #[must_use]
    pub fn execution(&self) -> &SimulationExecutionBounds {
        match self {
            Self::V0 { execution, .. } => execution,
        }
    }

    /// Return the exact model reference.
    #[must_use]
    pub fn model(&self) -> &SimulationModelReference {
        match self {
            Self::V0 { model, .. } => model,
        }
    }
}

/// Exact immutable robot bundle identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationBundleReference {
    /// Robot identity copied from the bundle manifest.
    pub robot_id: String,
    /// SHA-256 of the exact `manifest.json` bytes.
    pub manifest_sha256: String,
}

/// Exact native model selected for the run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationModelReference {
    /// Bundle-relative or project-relative authored scene identity.
    pub scene: String,
    /// Closed model identity returned by the simulator probe.
    pub model_identity: String,
}

/// Exact independently managed simulator application selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationApplicationReference {
    /// Cargo package identity of the simulator application.
    pub package: String,
    /// Exact application version.
    pub version: String,
    /// Binary target name.
    pub binary: String,
}

/// Canonical finite program embedded in the run specification.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationProgram {
    /// Canonical program bytes.
    pub bytes: Vec<u8>,
}

/// One observation or native-body requirement declared before admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum SimulationCaptureRequirement {
    /// A generated observation method.
    Observation {
        /// Configured runtime instance.
        instance: String,
        /// Generated observation signature.
        signature: MethodSignature,
        /// Retention policy selected by the author.
        policy: SimulationCapturePolicy,
    },
    /// Native truth for the model root body.
    RootBody {
        /// Requested body name, validated against the selected model.
        body: String,
        /// Sampling cadence in completed native boundaries.
        every_steps: u32,
    },
}

/// Artifact form of the SDK capture policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SimulationCapturePolicy {
    /// Retain only the newest publication.
    Latest,
    /// Retain a bounded history and expose any discarded prefix.
    BestEffortHistory {
        /// Maximum retained records.
        capacity: u32,
    },
    /// Require a complete history within the admitted capacity.
    RequiredHistory {
        /// Maximum admitted records before completeness fails.
        capacity: u32,
    },
}

/// Finite controlled-execution bounds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationExecutionBounds {
    /// Native simulator quantum.
    pub quantum_ns: u64,
    /// Exact transition count. No implicit transition is appended.
    pub transitions: u32,
    /// Host-monotonic execution deadline.
    pub host_deadline_ms: u64,
    /// Non-extendable graceful shutdown budget.
    pub shutdown_grace_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_specification_round_trips_without_a_bundle_scenario_record() {
        let specification = SimulationRunSpecification::V0 {
            bundle: SimulationBundleReference {
                robot_id: "robot".to_owned(),
                manifest_sha256: "a".repeat(64),
            },
            model: SimulationModelReference {
                scene: "simulation/scene.xml".to_owned(),
                model_identity: "model".to_owned(),
            },
            simulator: SimulationApplicationReference {
                package: "phoxal-simulator".to_owned(),
                version: "1.0.0".to_owned(),
                binary: "phoxal-simulator".to_owned(),
            },
            program: SimulationProgram {
                bytes: vec![1, 2, 3],
            },
            captures: Vec::new(),
            execution: SimulationExecutionBounds {
                quantum_ns: 2_000_000,
                transitions: 10,
                host_deadline_ms: 30_000,
                shutdown_grace_ms: 10_000,
            },
        };
        let json = serde_json::to_vec(&specification).expect("serialize");
        let decoded: SimulationRunSpecification =
            serde_json::from_slice(&json).expect("deserialize");
        assert_eq!(decoded, specification);
        assert!(!String::from_utf8(json).unwrap().contains("BundleScenario"));
    }
}
