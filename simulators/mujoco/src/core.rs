//! Presentation-neutral MuJoCo simulation coordination.
//!
//! The headless command and desktop presentation both drive this coordinator.
//! It owns the one controlled scene, performs one provider/native boundary at
//! a time, and never exposes native mutable data to a presentation.

use std::fmt;
use std::path::Path;

use phoxal_mujoco::{
    Boundary, ClosedModel, ControlledError, ControlledPhase, ControlledScene, ExecutionId,
    HoldProvider, Model, ModelError, PhysicsQuantum, Scene, SceneError, StateSnapshot, TimelineId,
};
use serde::Serialize;

const DURATION_TOLERANCE: f64 = 1.0e-9;

/// A finite run bound selected by the caller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RunBounds {
    /// Run exactly this many native transitions.
    Steps(u64),
    /// Run exactly this many native quanta represented as seconds.
    Duration(f64),
}

impl RunBounds {
    /// Resolves a finite bound against the model's source-authored quantum.
    pub fn resolve(self, quantum: PhysicsQuantum) -> Result<ResolvedBounds, BoundsError> {
        match self {
            Self::Steps(steps) if steps > 0 => {
                let duration_seconds = steps as f64 * quantum.as_seconds();
                if !duration_seconds.is_finite() {
                    return Err(BoundsError::StepDurationOverflow { steps });
                }
                Ok(ResolvedBounds {
                    steps,
                    duration_seconds,
                })
            }
            Self::Steps(_) => Err(BoundsError::NonPositiveSteps),
            Self::Duration(duration) => {
                if !duration.is_finite() || duration <= 0.0 {
                    return Err(BoundsError::InvalidDuration { duration });
                }
                let ratio = duration / quantum.as_seconds();
                let rounded = ratio.round();
                if !ratio.is_finite()
                    || rounded < 1.0
                    || rounded > u64::MAX as f64
                    || !close_enough(ratio, rounded)
                {
                    return Err(BoundsError::NotIntegral {
                        duration,
                        quantum: quantum.as_seconds(),
                    });
                }
                let steps = rounded as u64;
                let represented = steps as f64 * quantum.as_seconds();
                if !represented.is_finite() || !close_enough(represented, duration) {
                    return Err(BoundsError::NotIntegral {
                        duration,
                        quantum: quantum.as_seconds(),
                    });
                }
                Ok(ResolvedBounds {
                    steps,
                    duration_seconds: represented,
                })
            }
        }
    }
}

/// A validated finite bound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedBounds {
    /// Exact transition count.
    pub steps: u64,
    /// Duration represented by those transitions.
    pub duration_seconds: f64,
}

/// A finite-bound validation failure.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BoundsError {
    /// A step bound must contain at least one transition.
    #[error("step bound must be positive")]
    NonPositiveSteps,
    /// The informational duration for a step bound cannot be represented.
    #[error("step bound {steps} overflows the finite duration representation")]
    StepDurationOverflow {
        /// Requested transition count.
        steps: u64,
    },
    /// A duration bound must be positive and finite.
    #[error("duration {duration} must be positive and finite")]
    InvalidDuration {
        /// Invalid duration value.
        duration: f64,
    },
    /// A duration does not represent an integral number of native quanta.
    #[error("duration {duration} is not an integral number of native quanta {quantum}")]
    NotIntegral {
        /// Requested duration.
        duration: f64,
        /// Source-authored quantum.
        quantum: f64,
    },
}

/// A coordinator construction or finite-run request failure.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The model/resource closure could not be admitted.
    #[error("model artifact error: {0}")]
    Artifact(#[from] phoxal_mujoco::ArtifactError),
    /// The native model could not be compiled.
    #[error("model compilation error: {0}")]
    Model(#[from] ModelError),
    /// The scene could not be initialized or reset.
    #[error("scene error: {0}")]
    Scene(#[from] SceneError),
    /// The provider/native controlled boundary failed.
    #[error("controlled boundary error: {0}")]
    Controlled(#[from] ControlledError),
    /// The finite bound is invalid for the source-authored quantum.
    #[error("invalid finite run bound: {0}")]
    Bounds(#[from] BoundsError),
}

/// Machine-readable finite-run outcome.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// Every requested boundary reached complete provider admission.
    Success,
    /// The caller stopped at a completed or native-progress boundary.
    Cancelled,
    /// A required provider or native boundary failed.
    Failed,
}

/// Typed evidence for a failed or cancelled finite run.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RunFailure {
    /// Stable failure category.
    pub kind: String,
    /// Human-readable diagnostic retained from the owning boundary.
    pub message: String,
    /// Native scene boundary observed when the run stopped.
    pub native_boundary: u64,
}

/// Machine-readable terminal evidence for one finite run.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RunSummary {
    /// Summary schema identifier.
    pub schema: &'static str,
    /// Closed source/resource model identity.
    pub model_id: String,
    /// Execution identity.
    pub execution: String,
    /// Reset-scoped timeline identity.
    pub timeline: String,
    /// Source-authored native quantum.
    pub quantum_seconds: f64,
    /// Requested native transition count.
    pub requested_steps: u64,
    /// Requested duration when the caller selected duration mode.
    pub requested_duration_seconds: Option<f64>,
    /// Completed boundary when the finite run began.
    pub start_boundary: u64,
    /// Last native boundary reached, including unadmitted native progress.
    pub final_boundary: u64,
    /// Number of boundaries with complete provider/native admission.
    pub completed_steps: u64,
    /// Terminal outcome.
    pub outcome: RunOutcome,
    /// Failure or cancellation evidence, if any.
    pub failure: Option<RunFailure>,
}

/// The single native coordinator shared by all presentations.
pub struct SimulationCore<P = HoldProvider> {
    controlled: ControlledScene<P>,
    stop_requested: bool,
}

impl<P> fmt::Debug for SimulationCore<P>
where
    P: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SimulationCore")
            .field("controlled", &self.controlled)
            .field("stop_requested", &self.stop_requested)
            .finish()
    }
}

impl SimulationCore<HoldProvider> {
    /// Loads a model/resource closure from the explicit model path.
    pub fn from_model_path(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let artifact = ClosedModel::from_file(path)?;
        let model = Model::from_closed(artifact)?;
        Ok(Self::from_scene(Scene::new(model)?))
    }

    /// Creates a coordinator with the explicit native scene and a hold provider.
    #[must_use]
    pub fn from_scene(scene: Scene) -> Self {
        let execution_bytes = scene.model().identity().as_bytes();
        let mut execution = [0_u8; 16];
        execution.copy_from_slice(&execution_bytes[..16]);
        Self::with_provider(
            scene,
            HoldProvider::default(),
            ExecutionId::from_u128(u128::from_be_bytes(execution)),
            TimelineId::from_u128(1),
        )
    }
}

impl<P> SimulationCore<P> {
    /// Creates a coordinator with an explicitly owned provider and identities.
    #[must_use]
    pub fn with_provider(
        scene: Scene,
        provider: P,
        execution: ExecutionId,
        timeline: TimelineId,
    ) -> Self {
        Self {
            controlled: ControlledScene::new(scene, provider, execution, timeline),
            stop_requested: false,
        }
    }

    /// Returns the immutable model identity.
    #[must_use]
    pub fn model_id(&self) -> phoxal_mujoco::ModelIdentity {
        self.controlled.scene().model().identity()
    }

    /// Returns the execution identity.
    #[must_use]
    pub fn execution(&self) -> ExecutionId {
        self.controlled.execution()
    }

    /// Returns the current timeline identity.
    #[must_use]
    pub fn timeline(&self) -> TimelineId {
        self.controlled.timeline()
    }

    /// Returns the source-authored native quantum.
    #[must_use]
    pub fn quantum(&self) -> PhysicsQuantum {
        self.controlled.scene().quantum()
    }

    /// Returns the current scoped scene boundary.
    #[must_use]
    pub fn boundary(&self) -> Boundary {
        self.controlled.boundary()
    }

    /// Returns the coordinator phase.
    #[must_use]
    pub fn phase(&self) -> ControlledPhase {
        self.controlled.phase()
    }

    /// Returns the latest copied native state.
    pub fn snapshot(&self) -> Result<StateSnapshot, CoreError>
    where
        P: phoxal_mujoco::SimulationProvider,
    {
        Ok(self.controlled.snapshot()?)
    }

    /// Requests a stop at the next completed boundary.
    pub fn request_stop(&mut self) {
        self.stop_requested = true;
    }

    /// Clears a stop request before starting another healthy finite run.
    pub fn clear_stop(&mut self) {
        self.stop_requested = false;
    }
}

impl<P> SimulationCore<P>
where
    P: phoxal_mujoco::SimulationProvider,
{
    /// Advances exactly one complete provider/native boundary.
    pub fn step(&mut self) -> Result<(), ControlledError> {
        self.controlled.advance(1).map(|_| ())
    }

    /// Resets a healthy scene and returns its new boundary-zero state.
    pub fn reset_snapshot(&mut self) -> Result<StateSnapshot, ControlledError> {
        self.controlled.reset()
    }

    /// Runs a finite bound one boundary at a time and returns terminal evidence.
    ///
    /// One-boundary iteration gives a desktop presentation a stop/failure
    /// opportunity between native transitions and preserves the exact same
    /// provider admission path as headless execution.
    pub fn run_finite(&mut self, bounds: RunBounds) -> Result<RunSummary, CoreError> {
        let resolved = bounds.resolve(self.quantum())?;
        let start = self.boundary();
        let requested_duration_seconds = match bounds {
            RunBounds::Steps(_) => None,
            RunBounds::Duration(duration) => Some(duration),
        };
        let mut completed_steps = 0_u64;
        for _ in 0..resolved.steps {
            if self.stop_requested {
                return Ok(self.summary(
                    resolved,
                    requested_duration_seconds,
                    start,
                    completed_steps,
                    RunOutcome::Cancelled,
                    Some(RunFailure {
                        kind: "cancelled".to_owned(),
                        message: "stop requested at a completed boundary".to_owned(),
                        native_boundary: self.boundary().index(),
                    }),
                ));
            }
            match self.controlled.advance(1) {
                Ok(_) => completed_steps += 1,
                Err(error) => {
                    return Ok(self.summary(
                        resolved,
                        requested_duration_seconds,
                        start,
                        completed_steps,
                        RunOutcome::Failed,
                        Some(RunFailure {
                            kind: controlled_error_kind(&error).to_owned(),
                            message: error.to_string(),
                            native_boundary: self.boundary().index(),
                        }),
                    ));
                }
            }
        }
        Ok(self.summary(
            resolved,
            requested_duration_seconds,
            start,
            completed_steps,
            RunOutcome::Success,
            None,
        ))
    }

    fn summary(
        &self,
        resolved: ResolvedBounds,
        requested_duration_seconds: Option<f64>,
        start: Boundary,
        completed_steps: u64,
        outcome: RunOutcome,
        failure: Option<RunFailure>,
    ) -> RunSummary {
        RunSummary {
            schema: "phoxal/simulation-run/v0",
            model_id: self.model_id().to_string(),
            execution: self.execution().to_string(),
            timeline: self.timeline().to_string(),
            quantum_seconds: self.quantum().as_seconds(),
            requested_steps: resolved.steps,
            requested_duration_seconds,
            start_boundary: start.index(),
            final_boundary: self.boundary().index(),
            completed_steps,
            outcome,
            failure,
        }
    }
}

pub(crate) fn controlled_error_kind(error: &ControlledError) -> &'static str {
    match error {
        ControlledError::Scene(_) => "native_scene",
        ControlledError::ProviderPrepare { .. } => "provider_prepare",
        ControlledError::SelectionBoundary { .. } => "provider_selection_boundary",
        ControlledError::SelectionSource { .. } => "provider_selection_source",
        ControlledError::ProviderObserve { .. } => "provider_observe",
        ControlledError::ProviderReset { .. } => "provider_reset",
        ControlledError::ResetAfterFailure => "reset_after_failure",
        ControlledError::TimelineOverflow { .. } => "timeline_overflow",
        ControlledError::AdvanceAfterFailure { .. } => "advance_after_failure",
    }
}

fn close_enough(actual: f64, expected: f64) -> bool {
    let scale = actual.abs().max(expected.abs()).max(1.0);
    (actual - expected).abs() <= DURATION_TOLERANCE * scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal_mujoco::{ActuatorSelection, ObservationReceipt, PrepareRequest, ProviderReset, SourceId};

    const FIXTURE: &str = r#"
        <mujoco model="core-fixture">
          <option timestep="0.01"/>
          <worldbody><body name="arm"><joint name="hinge" type="hinge"/><geom type="sphere" size="0.05" mass="1"/></body></worldbody>
          <actuator><motor name="motor" joint="hinge" ctrlrange="-1 1" ctrllimited="true"/></actuator>
        </mujoco>
    "#;

    #[derive(Debug)]
    struct FailingProvider;

    impl phoxal_mujoco::SimulationProvider for FailingProvider {
        type Error = &'static str;

        fn prepare(
            &mut self,
            request: PrepareRequest<'_>,
        ) -> Result<ActuatorSelection, Self::Error> {
            Ok(ActuatorSelection::new(
                request.boundary(),
                SourceId::from_u128(1),
                request.state().controls().to_vec().into_boxed_slice(),
            ))
        }

        fn observe(&mut self, _receipt: ObservationReceipt<'_>) -> Result<(), Self::Error> {
            Err("required observation rejected")
        }

        fn reset(&mut self, _receipt: ProviderReset<'_>) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn scene() -> Scene {
        let model = Model::from_xml(FIXTURE).unwrap();
        Scene::new(model).unwrap()
    }

    #[test]
    fn duration_requires_integral_native_quanta() {
        let quantum = PhysicsQuantum::from_seconds(0.01).unwrap();
        assert_eq!(
            RunBounds::Duration(0.1).resolve(quantum).unwrap().steps,
            10
        );
        assert!(matches!(
            RunBounds::Duration(0.105).resolve(quantum),
            Err(BoundsError::NotIntegral { .. })
        ));
    }

    #[test]
    fn step_bound_rejects_unrepresentable_summary_duration() {
        let quantum = PhysicsQuantum::from_seconds(f64::MAX).unwrap();
        assert!(matches!(
            RunBounds::Steps(2).resolve(quantum),
            Err(BoundsError::StepDurationOverflow { steps: 2 })
        ));
    }

    #[test]
    fn failed_observation_reports_native_progress_without_success() {
        let mut core = SimulationCore::with_provider(
            scene(),
            FailingProvider,
            ExecutionId::from_u128(1),
            TimelineId::from_u128(1),
        );
        let summary = core.run_finite(RunBounds::Steps(2)).unwrap();
        assert_eq!(summary.outcome, RunOutcome::Failed);
        assert_eq!(summary.completed_steps, 0);
        assert_eq!(summary.final_boundary, 1);
        assert_eq!(summary.failure.as_ref().unwrap().kind, "provider_observe");
        assert_eq!(
            core.controlled.phase(),
            phoxal_mujoco::ControlledPhase::Failed
        );
    }

    #[test]
    fn finite_steps_report_exact_success_boundaries() {
        let mut core = SimulationCore::from_scene(scene());
        let summary = core.run_finite(RunBounds::Steps(3)).unwrap();
        assert_eq!(summary.outcome, RunOutcome::Success);
        assert_eq!(summary.requested_steps, 3);
        assert_eq!(summary.start_boundary, 0);
        assert_eq!(summary.final_boundary, 3);
        assert_eq!(summary.completed_steps, 3);
        assert!(summary.failure.is_none());
    }

    #[test]
    fn stop_is_reported_without_advancing_the_native_boundary() {
        let mut core = SimulationCore::from_scene(scene());
        core.stop_requested = true;
        let summary = core.run_finite(RunBounds::Steps(3)).unwrap();
        assert_eq!(summary.outcome, RunOutcome::Cancelled);
        assert_eq!(summary.start_boundary, 0);
        assert_eq!(summary.final_boundary, 0);
        assert_eq!(summary.completed_steps, 0);
        assert_eq!(summary.failure.as_ref().unwrap().kind, "cancelled");
    }
}
