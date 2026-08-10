//! Bundle readers for supervisors and participant processes.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use phoxal_model::Robot;
use phoxal_runtime_contract::identity::{ParticipantArtifactId, ParticipantId};

use crate::{
    BinaryReference, BundleError, BundleRoot, ParticipantAssets, RuntimeDocument,
    RuntimeParticipant, SelectionError, read_runtime_document, require_layout_directories,
    validate_layout,
};

/// One selected participant and the immutable runtime inputs it consumes.
#[derive(Clone, Debug)]
pub struct ParticipantRuntimeInputs {
    robot: Arc<Robot>,
    participant: RuntimeParticipant,
    /// The reusable artifact selected by `participant.artifact`.
    artifact: BinaryReference,
    assets: ParticipantAssets,
}

impl ParticipantRuntimeInputs {
    /// The canonical robot selected with this participant.
    #[must_use]
    pub fn robot(&self) -> &Robot {
        self.robot.as_ref()
    }

    /// The exact persisted participant record selected for this process.
    #[must_use]
    pub const fn participant(&self) -> &RuntimeParticipant {
        &self.participant
    }

    /// The reusable executable artifact selected by the participant record.
    #[must_use]
    pub const fn artifact(&self) -> &BinaryReference {
        &self.artifact
    }

    /// Participant-readable, digest-checked assets from the same bundle.
    #[must_use]
    pub const fn assets(&self) -> &ParticipantAssets {
        &self.assets
    }
}

/// A loaded, integrity-checked runtime bundle.
#[derive(Clone, Debug)]
pub struct RuntimeBundle {
    root: BundleRoot,
    document: RuntimeDocument,
    assets: ParticipantAssets,
}

impl RuntimeBundle {
    /// Open and verify every indexed file in one installed bundle.
    ///
    /// This is the supervisor/builder boundary: it rejects an unrelated
    /// changed asset or executable before an execution is created.
    pub fn open_verified(root: impl AsRef<Path>) -> Result<Self, BundleError> {
        let root = BundleRoot::open(root.as_ref())?;
        let document = read_runtime_document(&root)?;
        validate_layout(&root, document.runtime())?;
        Ok(Self {
            assets: ParticipantAssets::new(root.clone(), &document.runtime().assets),
            root,
            document,
        })
    }

    /// The requested installed root path, retained for diagnostics.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// The validated persisted document.
    #[must_use]
    pub const fn document(&self) -> &RuntimeDocument {
        &self.document
    }

    /// The canonical robot loaded from runtime.json, with no source parser.
    #[must_use]
    pub fn robot(&self) -> &Robot {
        self.document.robot()
    }

    /// The sole persisted RobotId.
    #[must_use]
    pub fn robot_id(&self) -> &phoxal_model::identity::RobotId {
        self.document.robot_id()
    }

    /// The final persisted participant set.
    #[must_use]
    pub fn participants(&self) -> &[RuntimeParticipant] {
        self.document.participants()
    }

    /// The reusable executable artifacts retained by this bundle.
    #[must_use]
    pub fn artifacts(&self) -> &BTreeMap<ParticipantArtifactId, BinaryReference> {
        self.document.artifacts()
    }

    /// Participant-readable digest-checked assets.
    #[must_use]
    pub const fn assets(&self) -> &ParticipantAssets {
        &self.assets
    }

    /// Select one exact participant record before opening any bus session.
    pub fn participant(&self, id: &ParticipantId) -> Result<&RuntimeParticipant, SelectionError> {
        self.document.participant(id)
    }

    /// Build one selected runtime-input object.
    pub fn participant_inputs(
        &self,
        id: &ParticipantId,
    ) -> Result<ParticipantRuntimeInputs, SelectionError> {
        selected_inputs(&self.document, self.assets.clone(), id)
    }

    pub(crate) fn relocated(mut self, path: std::path::PathBuf) -> Self {
        self.root.relocate(path.clone());
        self.assets.relocate(path);
        self
    }
}

/// The exact runtime record a participant process was launched to consume.
#[derive(Clone, Debug)]
pub struct ParticipantBundle {
    root: BundleRoot,
    inputs: ParticipantRuntimeInputs,
}

impl ParticipantBundle {
    /// Open one participant's selected runtime inputs without hashing unrelated
    /// indexed files. Binary integrity is the supervisor's concern: the daemon
    /// digest-verifies every staged executable when it opens the bundle, and a
    /// launched participant proves its identity through its embedded contract.
    pub fn open(root: impl AsRef<Path>, id: &ParticipantId) -> Result<Self, BundleError> {
        let root = BundleRoot::open(root.as_ref())?;
        let document = read_runtime_document(&root)?;
        require_layout_directories(&root)?;
        let assets = ParticipantAssets::new(root.clone(), &document.runtime().assets);
        let inputs = selected_inputs(&document, assets, id)?;
        Ok(Self { root, inputs })
    }

    /// The selected, coherent runtime inputs.
    #[must_use]
    pub const fn inputs(&self) -> &ParticipantRuntimeInputs {
        &self.inputs
    }

    /// Consume this selected bundle into its runtime inputs.
    #[must_use]
    pub fn into_inputs(self) -> ParticipantRuntimeInputs {
        self.inputs
    }

    /// The bundle root retained for diagnostics.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.path()
    }
}

fn selected_inputs(
    document: &RuntimeDocument,
    assets: ParticipantAssets,
    id: &ParticipantId,
) -> Result<ParticipantRuntimeInputs, SelectionError> {
    let participant = document.participant(id)?.clone();
    let artifact = document
        .artifacts()
        .get(&participant.artifact)
        .cloned()
        .ok_or_else(|| SelectionError::MissingArtifact {
            participant: participant.id.clone(),
            artifact: participant.artifact.clone(),
        })?;
    Ok(ParticipantRuntimeInputs {
        robot: Arc::new(document.robot().clone()),
        participant,
        artifact,
        assets,
    })
}
