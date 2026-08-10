//! Final participant-instance records.

use phoxal_model::identity::ComponentInstanceId;
use phoxal_model::{Clock, Robot};
use phoxal_runtime_contract::identity::{ParticipantArtifactId, ParticipantId};
use phoxal_runtime_contract::metadata::ParticipantKind;
use serde::{Deserialize, Serialize};

use crate::{BinaryReference, DocumentError, ParticipantClock};

/// One exact process entry in the final runtime graph.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeParticipant {
    pub(crate) id: ParticipantId,
    pub(crate) artifact: ParticipantArtifactId,
    pub(crate) config: Option<serde_json::Value>,
    pub(crate) component: Option<ComponentInstanceId>,
    pub(crate) clock: ParticipantClock,
}

impl RuntimeParticipant {
    #[must_use]
    pub fn new(
        id: ParticipantId,
        artifact: ParticipantArtifactId,
        config: Option<serde_json::Value>,
        component: Option<ComponentInstanceId>,
        clock: ParticipantClock,
    ) -> Self {
        Self {
            id,
            artifact,
            config,
            component,
            clock,
        }
    }
    #[must_use]
    pub const fn id(&self) -> &ParticipantId {
        &self.id
    }
    #[must_use]
    pub const fn artifact(&self) -> &ParticipantArtifactId {
        &self.artifact
    }
    #[must_use]
    pub fn config(&self) -> Option<&serde_json::Value> {
        self.config.as_ref()
    }
    #[must_use]
    pub const fn component(&self) -> Option<&ComponentInstanceId> {
        self.component.as_ref()
    }
    #[must_use]
    pub const fn clock(&self) -> ParticipantClock {
        self.clock
    }

    pub(crate) fn validate(
        &self,
        robot: &Robot,
        artifact: &BinaryReference,
        config_validator: &jsonschema::Validator,
    ) -> Result<(), DocumentError> {
        if !artifact.path().starts_with_directory(crate::BIN_DIR) {
            return Err(DocumentError::ArtifactOutsideBin {
                artifact: self.artifact.clone(),
                path: artifact.path().clone(),
            });
        }
        if let Some(component) = &self.component
            && robot.component_instance(component.as_str()).is_none()
        {
            return Err(DocumentError::UnknownComponent {
                participant: self.id.clone(),
                component_instance: component.clone(),
            });
        }
        match (artifact.contract().kind, &self.component) {
            (ParticipantKind::Driver, None) => {
                return Err(DocumentError::MissingDriverComponent {
                    participant: self.id.clone(),
                });
            }
            (ParticipantKind::Driver, Some(_)) | (_, None) => {}
            (kind, Some(component_instance)) => {
                return Err(DocumentError::UnexpectedComponent {
                    participant: self.id.clone(),
                    kind,
                    component_instance: component_instance.clone(),
                });
            }
        }
        let kind = artifact.contract().kind;
        let execution_mode_matches = match kind {
            ParticipantKind::Driver => {
                robot.clock() == Clock::Real && self.clock != ParticipantClock::Simulation
            }
            ParticipantKind::Simulator => {
                robot.clock() == Clock::Simulated && self.clock == ParticipantClock::Simulation
            }
            ParticipantKind::Brain | ParticipantKind::Service => true,
        };
        if !execution_mode_matches {
            return Err(DocumentError::ExecutionModeMismatch {
                participant: self.id.clone(),
                kind,
                robot: robot.clock(),
                participant_clock: self.clock,
            });
        }
        match (robot.clock(), self.clock) {
            (Clock::Real, ParticipantClock::Simulation) => {
                return Err(DocumentError::ClockMismatch {
                    participant: self.id.clone(),
                    robot: Clock::Real,
                    participant_clock: self.clock,
                });
            }
            (Clock::Simulated, ParticipantClock::Real) => {
                return Err(DocumentError::ClockMismatch {
                    participant: self.id.clone(),
                    robot: Clock::Simulated,
                    participant_clock: self.clock,
                });
            }
            _ => {}
        }
        let null = serde_json::Value::Null;
        let config = self.config.as_ref().unwrap_or(&null);
        if let Err(error) = config_validator.validate(config) {
            return Err(DocumentError::InvalidConfig {
                participant: self.id.clone(),
                error: error.to_string(),
            });
        }
        Ok(())
    }
}
