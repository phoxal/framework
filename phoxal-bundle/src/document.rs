//! Runtime document, asset index, and invariant validation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use phoxal_model::Robot;
use phoxal_model::component::capability::MotorCommand;
use phoxal_model::identity::CapabilityRef;
use phoxal_runtime_contract::identity::{ParticipantArtifactId, ParticipantId};
use phoxal_runtime_contract::metadata::{ParticipantContract, ParticipantRequirement};
use phoxal_runtime_contract::version::RobotApiVersion;
use serde::{Deserialize, Serialize};

use crate::{
    ASSETS_DIR, AssetIndex, BinaryReference, BundleError, BundlePath, DocumentError,
    RuntimeParticipant, SelectionError,
};

/// The scheduler policy persisted for one runtime participant instance.
///
/// This belongs to the compiled runtime bundle because it is a runtime
/// selection fact, not a process-contract/launch parser type.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantClock {
    /// Follow the host's boot-anchored real clock.
    Real,
    /// Follow the simulation world clock supplied by the runtime.
    Simulation,
    /// Do not schedule robot-time steps.
    Clockless,
}

impl fmt::Display for ParticipantClock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Real => "real",
            Self::Simulation => "simulation",
            Self::Clockless => "clockless",
        })
    }
}

/// A schema-tagged persisted runtime document.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum RuntimeDocument {
    /// The first runtime bundle schema. Older/future schemas are refused
    /// rather than guessed at by a runtime process.
    #[serde(rename = "phoxal/runtime-bundle/v0")]
    V0(Runtime),
}

impl<'de> Deserialize<'de> for RuntimeDocument {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "schema", deny_unknown_fields)]
        enum Wire {
            #[serde(rename = "phoxal/runtime-bundle/v0")]
            V0(Runtime),
        }

        match Wire::deserialize(deserializer)? {
            Wire::V0(runtime) => Ok(Self::new(runtime)),
        }
    }
}

impl RuntimeDocument {
    /// Wrap one already-validated runtime document.
    #[must_use]
    pub const fn new(runtime: Runtime) -> Self {
        Self::V0(runtime)
    }

    /// The runtime payload.
    #[must_use]
    pub const fn runtime(&self) -> &Runtime {
        match self {
            Self::V0(runtime) => runtime,
        }
    }

    /// The canonical robot identity persisted by this document.
    #[must_use]
    pub fn robot_id(&self) -> &phoxal_model::identity::RobotId {
        self.runtime().robot.id()
    }

    /// The canonical compiled robot.
    #[must_use]
    pub fn robot(&self) -> &Robot {
        &self.runtime().robot
    }

    /// The one Robot API revision validated for this execution.
    #[must_use]
    pub fn robot_api(&self) -> RobotApiVersion {
        self.runtime().robot_api()
    }

    /// The final participant set, in persisted order.
    #[must_use]
    pub fn participants(&self) -> &[RuntimeParticipant] {
        &self.runtime().participants
    }

    /// The reusable executable artifacts selected by participant instances.
    #[must_use]
    pub fn artifacts(&self) -> &BTreeMap<ParticipantArtifactId, BinaryReference> {
        &self.runtime().artifacts
    }

    /// Find the exact persisted participant selected by a process boundary.
    pub fn participant(&self, id: &ParticipantId) -> Result<&RuntimeParticipant, SelectionError> {
        self.participants()
            .iter()
            .find(|participant| participant.id == *id)
            .ok_or_else(|| SelectionError::Unknown {
                requested: id.clone(),
            })
    }
}

/// The persisted final runtime graph and all framework-owned runtime facts.
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    /// The complete canonical model. Its `id` is the sole persisted RobotId;
    /// there is no namespace or duplicate top-level identity field.
    pub(crate) robot: Robot,
    /// The reusable staged executables and their embedded compatibility
    /// contracts. Multiple participant instances may point to one entry.
    pub(crate) artifacts: BTreeMap<ParticipantArtifactId, BinaryReference>,
    /// The exact process instances the executor must launch, in final
    /// persisted form.
    pub(crate) participants: Vec<RuntimeParticipant>,
    /// The participant-readable asset index and integrity facts.
    pub(crate) assets: AssetIndex,
    /// Optional supervisor router configuration, kept as an indexed asset.
    pub(crate) router: Option<RuntimeRouterConfig>,
}

impl<'de> Deserialize<'de> for Runtime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            robot: Robot,
            artifacts: BTreeMap<ParticipantArtifactId, BinaryReference>,
            participants: Vec<RuntimeParticipant>,
            assets: AssetIndex,
            router: Option<RuntimeRouterConfig>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.robot,
            wire.artifacts,
            wire.participants,
            wire.assets,
            wire.router,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl Runtime {
    /// Construct the complete in-memory runtime document, validating its
    /// cross-field invariants exactly once.
    pub fn new(
        robot: Robot,
        artifacts: BTreeMap<ParticipantArtifactId, BinaryReference>,
        participants: Vec<RuntimeParticipant>,
        assets: AssetIndex,
        router: Option<RuntimeRouterConfig>,
    ) -> Result<Self, DocumentError> {
        let runtime = Self {
            robot,
            artifacts,
            participants,
            assets,
            router,
        };
        runtime.validate()?;
        Ok(runtime)
    }

    /// The canonical compiled robot.
    #[must_use]
    pub const fn robot(&self) -> &Robot {
        &self.robot
    }

    /// The reusable executable artifacts retained by this runtime.
    #[must_use]
    pub fn artifacts(&self) -> &BTreeMap<ParticipantArtifactId, BinaryReference> {
        &self.artifacts
    }

    /// The final participant set, in persisted order.
    #[must_use]
    pub fn participants(&self) -> &[RuntimeParticipant] {
        &self.participants
    }

    /// The participant-readable asset index.
    #[must_use]
    pub const fn assets(&self) -> &AssetIndex {
        &self.assets
    }

    /// Optional router configuration selected by build tooling.
    #[must_use]
    pub const fn router(&self) -> Option<&RuntimeRouterConfig> {
        self.router.as_ref()
    }

    /// The one Robot API revision selected by every launched participant.
    ///
    /// Runtime construction proves this invariant and every valid runtime has
    /// a brain participant, so the lookup cannot fail after validation.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Runtime is constructible only after validation proves at least one selected participant and its artifact"
    )]
    pub fn robot_api(&self) -> RobotApiVersion {
        self.participants
            .first()
            .and_then(|participant| self.artifacts.get(&participant.artifact))
            .map(|artifact| artifact.contract().api)
            .expect("validated runtime has a selected participant artifact")
    }

    fn validate(&self) -> Result<(), DocumentError> {
        let mut ids = BTreeSet::new();
        let mut artifact_paths = BTreeSet::new();
        let mut validators = BTreeMap::new();
        for (id, artifact) in &self.artifacts {
            artifact.validate(id)?;
            if artifact.contract().kind == phoxal_runtime_contract::metadata::ParticipantKind::Brain
            {
                if id.as_str() != "brain" {
                    return Err(DocumentError::BrainArtifactId { actual: id.clone() });
                }
                if artifact.path().as_str() != "bin/brain" {
                    return Err(DocumentError::BrainArtifactPath {
                        actual: artifact.path().clone(),
                    });
                }
            }
            if !artifact_paths.insert(artifact.path.clone()) {
                return Err(DocumentError::DuplicateBinary {
                    path: artifact.path.clone(),
                });
            }
            let validator =
                jsonschema::validator_for(&artifact.contract.config_schema).map_err(|error| {
                    DocumentError::InvalidConfigSchema {
                        artifact: id.clone(),
                        error: error.to_string(),
                    }
                })?;
            validate_requirement(artifact.contract(), id, &self.robot)?;
            validators.insert(id, validator);
        }
        let mut referenced_artifacts = BTreeSet::new();
        let mut brain = None;
        let mut simulator_count = 0_u8;
        let mut robot_api = None;
        for participant in &self.participants {
            let artifact = self.artifacts.get(&participant.artifact).ok_or_else(|| {
                DocumentError::UnknownArtifact {
                    participant: participant.id.clone(),
                    artifact: participant.artifact.clone(),
                }
            })?;
            let validator = validators.get(&participant.artifact).ok_or_else(|| {
                DocumentError::UnknownArtifact {
                    participant: participant.id.clone(),
                    artifact: participant.artifact.clone(),
                }
            })?;
            participant.validate(&self.robot, artifact, validator)?;
            let api = artifact.contract().api;
            if let Some(expected) = robot_api
                && expected != api
            {
                return Err(DocumentError::MixedRobotApi {
                    artifact: participant.artifact.clone(),
                    expected,
                    actual: api,
                });
            }
            robot_api = Some(api);
            if artifact.contract().kind == phoxal_runtime_contract::metadata::ParticipantKind::Brain
            {
                if participant.id.as_str() != "brain" {
                    return Err(DocumentError::BrainIdMismatch {
                        actual: participant.id.clone(),
                    });
                }
                if brain.replace(participant.id.clone()).is_some() {
                    return Err(DocumentError::DuplicateBrain);
                }
            }
            if artifact.contract().kind
                == phoxal_runtime_contract::metadata::ParticipantKind::Simulator
            {
                simulator_count = simulator_count.saturating_add(1);
            }
            referenced_artifacts.insert(participant.artifact.clone());
            if !ids.insert(participant.id.clone()) {
                return Err(DocumentError::DuplicateParticipant {
                    id: participant.id.clone(),
                });
            }
        }
        if brain.is_none() {
            return Err(DocumentError::MissingBrain);
        }
        if self.robot.clock() == phoxal_model::Clock::Simulated {
            match simulator_count {
                0 => return Err(DocumentError::MissingSimulator),
                1 => {}
                _ => return Err(DocumentError::DuplicateSimulator),
            }
        }
        if let Some(artifact) = self
            .artifacts
            .keys()
            .find(|id| !referenced_artifacts.contains(*id))
        {
            return Err(DocumentError::UnusedArtifact {
                artifact: artifact.clone(),
            });
        }
        self.assets.validate()?;
        if let Some(router) = &self.router {
            router.validate(&self.assets)?;
        }
        Ok(())
    }
}

/// Validate one artifact's static topology requirement against the canonical
/// robot once, independently of how many runtime instances select it.
pub(crate) fn validate_requirement(
    contract: &ParticipantContract,
    artifact: &ParticipantArtifactId,
    robot: &Robot,
) -> Result<(), DocumentError> {
    let Some(requirement) = contract.requirement else {
        return Ok(());
    };
    match requirement {
        ParticipantRequirement::DifferentialDriveVelocity => {
            let phoxal_model::robot::KinematicConfig::Differential {
                left_actuators,
                right_actuators,
                ..
            } = robot.motion().kinematic()
            else {
                return Err(DocumentError::RequirementKinematicsMismatch {
                    artifact: artifact.clone(),
                    requirement,
                    actual: robot.motion().kinematic().kind(),
                });
            };
            validate_drive_side(artifact, "left_actuators", left_actuators, robot)?;
            validate_drive_side(artifact, "right_actuators", right_actuators, robot)
        }
    }
}

fn validate_drive_side(
    artifact: &ParticipantArtifactId,
    side: &'static str,
    actuators: &[CapabilityRef],
    robot: &Robot,
) -> Result<(), DocumentError> {
    if actuators.is_empty() {
        return Err(DocumentError::RequirementActuatorListEmpty {
            artifact: artifact.clone(),
            side,
        });
    }
    for reference in actuators {
        let (motor, _) = robot.require_motor(reference).map_err(|error| {
            DocumentError::RequirementActuatorInvalid {
                artifact: artifact.clone(),
                actuator: reference.clone(),
                error: error.to_string(),
            }
        })?;
        if motor.command != MotorCommand::Velocity {
            return Err(DocumentError::RequirementMotorModeMismatch {
                artifact: artifact.clone(),
                actuator: reference.clone(),
                expected: MotorCommand::Velocity,
                actual: motor.command,
            });
        }
    }
    Ok(())
}

/// A normalized reference to optional router configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRouterConfig {
    /// The config is an indexed asset, never an arbitrary filesystem path.
    path: BundlePath,
}

impl RuntimeRouterConfig {
    /// Construct router configuration pointing at one bundle asset.
    #[must_use]
    pub const fn new(path: BundlePath) -> Self {
        Self { path }
    }

    /// The indexed asset path containing the router configuration.
    #[must_use]
    pub const fn path(&self) -> &BundlePath {
        &self.path
    }

    fn validate(&self, assets: &AssetIndex) -> Result<(), DocumentError> {
        if !self.path.starts_with_directory(ASSETS_DIR) {
            return Err(DocumentError::RouterOutsideAssets {
                path: self.path.clone(),
            });
        }
        if !assets.entries.iter().any(|entry| entry.path == self.path) {
            return Err(DocumentError::RouterMissingAsset {
                path: self.path.clone(),
            });
        }
        Ok(())
    }
}

/// Decode the one schema-tagged document retained in an installed bundle.
pub(crate) fn decode(bytes: &[u8]) -> Result<RuntimeDocument, BundleError> {
    serde_json::from_slice(bytes).map_err(BundleError::from)
}
