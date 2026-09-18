//! Controlled provider exchange around the authoritative native scene.
//!
//! The provider boundary is deliberately small and synchronous.  A simulator
//! owns the scene and calls a supervisor/provider for one complete actuator
//! cut before each native transition, then reports the resulting observation
//! cut after the transition.  The provider never receives native pointers or
//! mutable scene state.

use std::fmt;

use crate::{PhysicsQuantum, Scene, SceneError, StateSnapshot};

/// A process-scoped execution identity.
///
/// The bytes are opaque to this crate.  Keeping the identity in every
/// boundary prevents a delayed provider receipt from being accepted by a
/// different execution.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExecutionId([u8; 16]);

impl ExecutionId {
    /// Creates an identity from an opaque value.
    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }

    /// Returns the opaque identity bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Display for ExecutionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// A reset-scoped timeline identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TimelineId([u8; 16]);

impl TimelineId {
    /// Creates a timeline identity from an opaque value.
    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }

    /// Returns the opaque identity bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }

    fn next(self) -> Option<Self> {
        let value = u128::from_be_bytes(self.0).checked_add(1)?;
        Some(Self::from_u128(value))
    }
}

impl fmt::Display for TimelineId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// An opaque provider/source identity attached to an actuator cut.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceId([u8; 16]);

impl SourceId {
    /// Creates a source identity from an opaque value.
    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }

    /// Returns the opaque identity bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// One completed scene boundary, scoped to one execution and timeline.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Boundary {
    execution: ExecutionId,
    timeline: TimelineId,
    index: u64,
}

impl Boundary {
    /// Creates a boundary identity.
    #[must_use]
    pub const fn new(execution: ExecutionId, timeline: TimelineId, index: u64) -> Self {
        Self {
            execution,
            timeline,
            index,
        }
    }

    /// Returns the execution identity.
    #[must_use]
    pub const fn execution(self) -> ExecutionId {
        self.execution
    }

    /// Returns the timeline identity.
    #[must_use]
    pub const fn timeline(self) -> TimelineId {
        self.timeline
    }

    /// Returns the completed boundary number.
    #[must_use]
    pub const fn index(self) -> u64 {
        self.index
    }
}

impl fmt::Display for Boundary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "execution {} timeline {} boundary {}",
            self.execution, self.timeline, self.index
        )
    }
}

/// Immutable input supplied to a provider before a native transition.
#[derive(Clone, Copy, Debug)]
pub struct PrepareRequest<'a> {
    boundary: Boundary,
    quantum: PhysicsQuantum,
    state: &'a StateSnapshot,
}

impl<'a> PrepareRequest<'a> {
    /// Creates a provider preparation request.
    #[must_use]
    pub const fn new(
        boundary: Boundary,
        quantum: PhysicsQuantum,
        state: &'a StateSnapshot,
    ) -> Self {
        Self {
            boundary,
            quantum,
            state,
        }
    }

    /// Returns the boundary whose action cut is being prepared.
    #[must_use]
    pub const fn boundary(self) -> Boundary {
        self.boundary
    }

    /// Returns the source-authored native quantum.
    #[must_use]
    pub const fn quantum(self) -> PhysicsQuantum {
        self.quantum
    }

    /// Returns the immutable state at the boundary-entry cut.
    #[must_use]
    pub const fn state(self) -> &'a StateSnapshot {
        self.state
    }
}

/// An immutable actuator selection for one exact boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct ActuatorSelection {
    boundary: Boundary,
    source: SourceId,
    controls: Box<[f64]>,
}

impl ActuatorSelection {
    /// Creates a selection.  The coordinator validates its boundary, length,
    /// finiteness, and native limits before mutating the scene.
    #[must_use]
    pub fn new(boundary: Boundary, source: SourceId, controls: impl Into<Box<[f64]>>) -> Self {
        Self {
            boundary,
            source,
            controls: controls.into(),
        }
    }

    /// Returns the exact boundary for which this selection was produced.
    #[must_use]
    pub const fn boundary(&self) -> Boundary {
        self.boundary
    }

    /// Returns the source that owns this selection.
    #[must_use]
    pub const fn source(&self) -> SourceId {
        self.source
    }

    /// Returns the immutable scalar native controls.
    #[must_use]
    pub fn controls(&self) -> &[f64] {
        &self.controls
    }
}

/// Immutable observation receipt delivered after one native transition.
#[derive(Clone, Copy, Debug)]
pub struct ObservationReceipt<'a> {
    boundary: Boundary,
    state: &'a StateSnapshot,
}

impl<'a> ObservationReceipt<'a> {
    /// Creates an observation receipt.
    #[must_use]
    pub const fn new(boundary: Boundary, state: &'a StateSnapshot) -> Self {
        Self { boundary, state }
    }

    /// Returns the completed boundary.
    #[must_use]
    pub const fn boundary(self) -> Boundary {
        self.boundary
    }

    /// Returns the copied post-transition state.
    #[must_use]
    pub const fn state(self) -> &'a StateSnapshot {
        self.state
    }
}

/// Reset receipt supplied to the provider after native data has been reset.
#[derive(Clone, Copy, Debug)]
pub struct ProviderReset<'a> {
    previous: Boundary,
    next: Boundary,
    state: &'a StateSnapshot,
}

impl<'a> ProviderReset<'a> {
    /// Creates a reset receipt.
    #[must_use]
    pub const fn new(previous: Boundary, next: Boundary, state: &'a StateSnapshot) -> Self {
        Self {
            previous,
            next,
            state,
        }
    }

    /// Returns the old timeline's last boundary.
    #[must_use]
    pub const fn previous(self) -> Boundary {
        self.previous
    }

    /// Returns the fresh boundary-zero identity.
    #[must_use]
    pub const fn next(self) -> Boundary {
        self.next
    }

    /// Returns the initial native state after reset.
    #[must_use]
    pub const fn state(self) -> &'a StateSnapshot {
        self.state
    }
}

/// Provider implementation used by a run that has no external actuator
/// source.  It explicitly holds the scene's previous control vector rather
/// than inventing a nominal value.
#[derive(Clone, Copy, Debug)]
pub struct HoldProvider {
    source: SourceId,
}

impl Default for HoldProvider {
    fn default() -> Self {
        Self {
            source: SourceId::from_u128(1),
        }
    }
}

impl HoldProvider {
    /// Creates a hold provider attributed to `source`.
    #[must_use]
    pub const fn new(source: SourceId) -> Self {
        Self { source }
    }
}

impl SimulationProvider for HoldProvider {
    type Error = std::convert::Infallible;

    fn prepare(&mut self, request: PrepareRequest<'_>) -> Result<ActuatorSelection, Self::Error> {
        Ok(ActuatorSelection::new(
            request.boundary(),
            self.source,
            request.state().controls().to_vec().into_boxed_slice(),
        ))
    }

    fn observe(&mut self, _receipt: ObservationReceipt<'_>) -> Result<(), Self::Error> {
        Ok(())
    }

    fn reset(&mut self, _receipt: ProviderReset<'_>) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// The provider side of the public simulation boundary.
pub trait SimulationProvider {
    /// Provider failure type.  Failures are retained as diagnostics by the
    /// coordinator, so providers need not expose their error type on the wire.
    type Error: fmt::Display;

    /// Prepare the complete immutable actuator cut for one boundary.
    fn prepare(&mut self, request: PrepareRequest<'_>) -> Result<ActuatorSelection, Self::Error>;

    /// Admit the complete copied observation cut after native integration.
    fn observe(&mut self, receipt: ObservationReceipt<'_>) -> Result<(), Self::Error>;

    /// Admit a fresh boundary-zero observation after a same-model reset.
    fn reset(&mut self, receipt: ProviderReset<'_>) -> Result<(), Self::Error>;
}

/// Coordinator lifecycle independent of native scene progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlledPhase {
    /// No transition is in flight.
    Paused,
    /// A bounded advance is collecting provider/native receipts.
    Running,
    /// The current execution is terminally failed.
    Failed,
}

/// One complete provider/native controlled advance.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlledStep {
    /// Starting completed boundary.
    pub start: Boundary,
    /// Ending completed boundary.
    pub end: Boundary,
    /// Copied native state after the final admitted observation cut.
    pub state: StateSnapshot,
}

/// Failure at the controlled scene/provider boundary.
#[derive(Debug, thiserror::Error)]
pub enum ControlledError {
    /// Native scene failure before the controlled advance completed.
    #[error(transparent)]
    Scene(#[from] SceneError),
    /// Provider preparation failed before native mutation.
    #[error("provider preparation failed at {boundary}: {detail}")]
    ProviderPrepare { boundary: Boundary, detail: String },
    /// Provider returned a selection for the wrong boundary.
    #[error("provider selection is for {found}, expected {expected}")]
    SelectionBoundary { found: Boundary, expected: Boundary },
    /// Provider returned a selection from an unauthorized source identity.
    #[error("provider selection source is {found}, expected {expected}")]
    SelectionSource { found: SourceId, expected: SourceId },
    /// Provider observation admission failed after native mutation.
    #[error(
        "provider observation admission failed at {boundary}: {detail}; native progress reached boundary {native_boundary}"
    )]
    ProviderObserve {
        boundary: Boundary,
        native_boundary: u64,
        state: Box<StateSnapshot>,
        detail: String,
    },
    /// Provider reset admission failed after native state mutation.
    #[error("provider reset admission failed at {boundary}: {detail}")]
    ProviderReset { boundary: Boundary, detail: String },
    /// A reset was requested for the terminal execution.
    #[error("controlled execution is terminally failed and cannot be reset")]
    ResetAfterFailure,
    /// Timeline identity allocation overflowed.
    #[error("timeline identity overflow while resetting {timeline}")]
    TimelineOverflow { timeline: TimelineId },
    /// A new transition was requested for the terminal execution.
    #[error("controlled execution is terminally failed at boundary {boundary}")]
    AdvanceAfterFailure { boundary: Boundary },
}

/// One native scene with one provider-bound controlled execution.
pub struct ControlledScene<P> {
    scene: Scene,
    provider: P,
    execution: ExecutionId,
    timeline: TimelineId,
    source: SourceId,
    phase: ControlledPhase,
}

impl<P> fmt::Debug for ControlledScene<P>
where
    P: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlledScene")
            .field("scene", &self.scene)
            .field("provider", &self.provider)
            .field("execution", &self.execution)
            .field("timeline", &self.timeline)
            .field("source", &self.source)
            .field("phase", &self.phase)
            .finish()
    }
}

impl<P> ControlledScene<P> {
    /// Creates a paused controlled run at the scene's current boundary.
    pub fn new(scene: Scene, provider: P, execution: ExecutionId, timeline: TimelineId) -> Self {
        Self::with_source(scene, provider, execution, timeline, SourceId::from_u128(1))
    }

    /// Creates a paused controlled run with an explicit provider source.
    #[must_use]
    pub fn with_source(
        scene: Scene,
        provider: P,
        execution: ExecutionId,
        timeline: TimelineId,
        source: SourceId,
    ) -> Self {
        Self {
            scene,
            provider,
            execution,
            timeline,
            source,
            phase: ControlledPhase::Paused,
        }
    }

    /// Returns the authoritative scene model.
    #[must_use]
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Returns the provider without exposing native scene data.
    #[must_use]
    pub fn provider(&self) -> &P {
        &self.provider
    }

    /// Returns the mutable provider owner.
    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }

    /// Returns the execution identity.
    #[must_use]
    pub const fn execution(&self) -> ExecutionId {
        self.execution
    }

    /// Returns the current timeline identity.
    #[must_use]
    pub const fn timeline(&self) -> TimelineId {
        self.timeline
    }

    /// Returns the provider source authorized for actuator selections.
    #[must_use]
    pub const fn source(&self) -> SourceId {
        self.source
    }

    /// Returns the current controlled lifecycle phase.
    #[must_use]
    pub const fn phase(&self) -> ControlledPhase {
        self.phase
    }

    /// Returns the current scoped boundary identity.
    #[must_use]
    pub fn boundary(&self) -> Boundary {
        Boundary::new(self.execution, self.timeline, self.scene.boundary())
    }

    /// Returns the latest copied native state.
    pub fn snapshot(&self) -> Result<StateSnapshot, ControlledError> {
        Ok(self.scene.snapshot()?)
    }
}

impl<P> ControlledScene<P>
where
    P: SimulationProvider,
{
    /// Advances exactly `count` complete provider/native boundaries.
    ///
    /// The provider must return the complete actuator cut before native data
    /// is mutated.  A post-step observation failure is terminal and retains
    /// the copied native state in [`ControlledError::ProviderObserve`].
    pub fn advance(&mut self, count: u64) -> Result<ControlledStep, ControlledError> {
        if self.phase == ControlledPhase::Failed {
            return Err(ControlledError::AdvanceAfterFailure {
                boundary: self.boundary(),
            });
        }
        if count == 0 {
            return Err(ControlledError::Scene(SceneError::ZeroAdvance));
        }
        let start = self.boundary();
        self.scene
            .boundary()
            .checked_add(count)
            .ok_or(ControlledError::Scene(SceneError::BoundaryOverflow {
                start: start.index(),
                count,
            }))?;
        self.phase = ControlledPhase::Running;

        for _ in 0..count {
            let boundary = self.boundary();
            let state = match self.scene.snapshot() {
                Ok(state) => state,
                Err(error) => {
                    self.phase = ControlledPhase::Failed;
                    return Err(ControlledError::Scene(error));
                }
            };
            let selection = match self.provider.prepare(PrepareRequest::new(
                boundary,
                self.scene.quantum(),
                &state,
            )) {
                Ok(selection) => selection,
                Err(error) => {
                    self.phase = ControlledPhase::Failed;
                    return Err(ControlledError::ProviderPrepare {
                        boundary,
                        detail: error.to_string(),
                    });
                }
            };
            if selection.boundary() != boundary {
                self.phase = ControlledPhase::Failed;
                return Err(ControlledError::SelectionBoundary {
                    found: selection.boundary(),
                    expected: boundary,
                });
            }
            if selection.source() != self.source {
                self.phase = ControlledPhase::Failed;
                return Err(ControlledError::SelectionSource {
                    found: selection.source(),
                    expected: self.source,
                });
            }

            let _native_step = match self.scene.integrate_controls(selection.controls()) {
                Ok(step) => step,
                Err(error) => {
                    self.phase = ControlledPhase::Failed;
                    return Err(ControlledError::Scene(error));
                }
            };
            let completed_boundary = self.boundary();
            let completed_state = match self.scene.snapshot() {
                Ok(state) => state,
                Err(error) => {
                    self.phase = ControlledPhase::Failed;
                    return Err(ControlledError::Scene(error));
                }
            };
            if let Err(error) = self.provider.observe(ObservationReceipt::new(
                completed_boundary,
                &completed_state,
            )) {
                self.phase = ControlledPhase::Failed;
                return Err(ControlledError::ProviderObserve {
                    boundary: completed_boundary,
                    native_boundary: completed_state.boundary(),
                    state: Box::new(completed_state),
                    detail: error.to_string(),
                });
            }
        }

        self.phase = ControlledPhase::Paused;
        let state = self.scene.snapshot()?;
        Ok(ControlledStep {
            start,
            end: self.boundary(),
            state,
        })
    }

    /// Resets a healthy run to boundary zero and rotates its timeline.
    ///
    /// Reset is intentionally unavailable after failure.  A failed native or
    /// provider owner requires a fresh [`ControlledScene`] and execution
    /// identity, so a caller cannot accidentally reuse stale authority.
    pub fn reset(&mut self) -> Result<StateSnapshot, ControlledError> {
        if self.phase == ControlledPhase::Failed {
            return Err(ControlledError::ResetAfterFailure);
        }
        let previous = self.boundary();
        let next_timeline = self
            .timeline
            .next()
            .ok_or(ControlledError::TimelineOverflow {
                timeline: self.timeline,
            })?;
        self.phase = ControlledPhase::Running;
        let state = match self.scene.reset() {
            Ok(state) => state,
            Err(error) => {
                self.phase = ControlledPhase::Failed;
                return Err(ControlledError::Scene(error));
            }
        };
        let next = Boundary::new(self.execution, next_timeline, 0);
        if let Err(error) = self
            .provider
            .reset(ProviderReset::new(previous, next, &state))
        {
            self.phase = ControlledPhase::Failed;
            return Err(ControlledError::ProviderReset {
                boundary: next,
                detail: error.to_string(),
            });
        }
        self.timeline = next_timeline;
        self.phase = ControlledPhase::Paused;
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClosedModel, Model};

    const FIXTURE: &str = r#"
        <mujoco model="provider-fixture">
          <option timestep="0.01"/>
          <worldbody><body name="arm"><joint name="hinge" type="hinge"/><geom type="sphere" size="0.05" mass="1"/></body></worldbody>
          <actuator><motor name="motor" joint="hinge" ctrlrange="-1 1" ctrllimited="true"/></actuator>
        </mujoco>
    "#;

    #[derive(Debug)]
    struct RecordingProvider {
        prepared: Vec<u64>,
        observed: Vec<u64>,
        fail_observe: bool,
        source: SourceId,
    }

    impl SimulationProvider for RecordingProvider {
        type Error = &'static str;

        fn prepare(
            &mut self,
            request: PrepareRequest<'_>,
        ) -> Result<ActuatorSelection, Self::Error> {
            self.prepared.push(request.boundary().index());
            Ok(ActuatorSelection::new(
                request.boundary(),
                self.source,
                request.state().controls().to_vec().into_boxed_slice(),
            ))
        }

        fn observe(&mut self, receipt: ObservationReceipt<'_>) -> Result<(), Self::Error> {
            self.observed.push(receipt.boundary().index());
            if self.fail_observe {
                Err("observation rejected")
            } else {
                Ok(())
            }
        }

        fn reset(&mut self, _receipt: ProviderReset<'_>) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn scene() -> Scene {
        let artifact = ClosedModel::from_xml(FIXTURE).unwrap();
        Scene::new(Model::from_closed(artifact).unwrap()).unwrap()
    }

    #[test]
    fn controlled_advance_collects_each_intermediate_boundary() {
        let provider = RecordingProvider {
            prepared: Vec::new(),
            observed: Vec::new(),
            fail_observe: false,
            source: SourceId::from_u128(1),
        };
        let mut controlled = ControlledScene::new(
            scene(),
            provider,
            ExecutionId::from_u128(10),
            TimelineId::from_u128(20),
        );
        let step = controlled.advance(3).unwrap();
        assert_eq!(step.start.index(), 0);
        assert_eq!(step.end.index(), 3);
        assert_eq!(controlled.provider().prepared, [0, 1, 2]);
        assert_eq!(controlled.provider().observed, [1, 2, 3]);
    }

    #[test]
    fn observation_failure_retains_native_progress_and_is_terminal() {
        let provider = RecordingProvider {
            prepared: Vec::new(),
            observed: Vec::new(),
            fail_observe: true,
            source: SourceId::from_u128(1),
        };
        let mut controlled = ControlledScene::new(
            scene(),
            provider,
            ExecutionId::from_u128(10),
            TimelineId::from_u128(20),
        );
        let error = controlled.advance(1).unwrap_err();
        assert!(matches!(
            error,
            ControlledError::ProviderObserve {
                native_boundary: 1,
                ..
            }
        ));
        assert_eq!(controlled.scene().boundary(), 1);
        assert_eq!(controlled.phase(), ControlledPhase::Failed);
        assert!(matches!(
            controlled.advance(1),
            Err(ControlledError::AdvanceAfterFailure { .. })
        ));
        assert!(matches!(
            controlled.reset(),
            Err(ControlledError::ResetAfterFailure)
        ));
    }

    #[test]
    fn healthy_reset_rotates_timeline_and_returns_zero_state() {
        let provider = RecordingProvider {
            prepared: Vec::new(),
            observed: Vec::new(),
            fail_observe: false,
            source: SourceId::from_u128(1),
        };
        let mut controlled = ControlledScene::new(
            scene(),
            provider,
            ExecutionId::from_u128(10),
            TimelineId::from_u128(20),
        );
        controlled.advance(2).unwrap();
        let state = controlled.reset().unwrap();
        assert_eq!(state.boundary(), 0);
        assert_eq!(controlled.boundary().index(), 0);
        assert_eq!(controlled.timeline(), TimelineId::from_u128(21));
        assert_eq!(controlled.phase(), ControlledPhase::Paused);
    }

    #[test]
    fn selection_from_another_source_is_rejected_before_native_progress() {
        let provider = RecordingProvider {
            prepared: Vec::new(),
            observed: Vec::new(),
            fail_observe: false,
            source: SourceId::from_u128(2),
        };
        let mut controlled = ControlledScene::new(
            scene(),
            provider,
            ExecutionId::from_u128(10),
            TimelineId::from_u128(20),
        );
        assert!(matches!(
            controlled.advance(1),
            Err(ControlledError::SelectionSource { .. })
        ));
        assert_eq!(controlled.scene().boundary(), 0);
        assert_eq!(controlled.phase(), ControlledPhase::Failed);
    }

    #[test]
    fn explicit_source_allows_a_configured_provider_identity() {
        let source = SourceId::from_u128(2);
        let provider = RecordingProvider {
            prepared: Vec::new(),
            observed: Vec::new(),
            fail_observe: false,
            source,
        };
        let mut controlled = ControlledScene::with_source(
            scene(),
            provider,
            ExecutionId::from_u128(10),
            TimelineId::from_u128(20),
            source,
        );
        controlled.advance(1).unwrap();
        assert_eq!(controlled.boundary().index(), 1);
    }
}
