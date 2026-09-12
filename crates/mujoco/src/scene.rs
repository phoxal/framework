//! Owned mutable workspaces and deterministic fixed-step scene execution.

use std::marker::PhantomData;
use std::rc::Rc;

use mujoco_rs::prelude::MjData;

use crate::error::{SceneError, WorkspaceError};
use crate::model::Model;

const TIME_TOLERANCE: f64 = 1.0e-9;

/// A positive source-authored native physics quantum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicsQuantum(f64);

impl PhysicsQuantum {
    /// Creates a quantum from a positive finite number of seconds.
    ///
    /// The value is retained as an `f64` because that is MuJoCo's native time
    /// representation.
    pub fn from_seconds(seconds: f64) -> Result<Self, SceneError> {
        if seconds.is_finite() && seconds > 0.0 {
            Ok(Self(seconds))
        } else {
            Err(SceneError::Model(
                crate::error::ModelError::InvalidTimestep(seconds),
            ))
        }
    }

    /// Returns the quantum in native seconds.
    #[must_use]
    pub const fn as_seconds(self) -> f64 {
        self.0
    }
}

/// Current lifecycle phase of an owned scene.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenePhase {
    /// No native transition is in flight.
    Paused,
    /// A caller is advancing the scene through one or more transitions.
    Running,
    /// The scene cannot be advanced until a new scene is created.
    Failed,
}

/// A copied observation of native state at one completed boundary.
///
/// The snapshot contains values, not pointers or borrowed native arrays.
/// It is therefore safe to hand to presentation or recording code after the
/// scene owner returns to its event loop.
#[derive(Clone, Debug, PartialEq)]
pub struct StateSnapshot {
    boundary: u64,
    time_seconds: f64,
    qpos: Box<[f64]>,
    qvel: Box<[f64]>,
    controls: Box<[f64]>,
    sensor_data: Box<[f64]>,
    body_positions: Box<[[f64; 3]]>,
    body_orientations: Box<[[f64; 4]]>,
}

impl StateSnapshot {
    /// Returns the completed logical boundary represented by this snapshot.
    #[must_use]
    pub const fn boundary(&self) -> u64 {
        self.boundary
    }

    /// Returns the native simulation time in seconds.
    #[must_use]
    pub const fn time_seconds(&self) -> f64 {
        self.time_seconds
    }

    /// Returns generalized positions.
    #[must_use]
    pub fn qpos(&self) -> &[f64] {
        &self.qpos
    }

    /// Returns generalized velocities.
    #[must_use]
    pub fn qvel(&self) -> &[f64] {
        &self.qvel
    }

    /// Returns scalar actuator controls selected for the current/next step.
    #[must_use]
    pub fn controls(&self) -> &[f64] {
        &self.controls
    }

    /// Returns the flattened native sensor data array.
    #[must_use]
    pub fn sensor_data(&self) -> &[f64] {
        &self.sensor_data
    }

    /// Returns post-forward Cartesian body positions.
    #[must_use]
    pub fn body_positions(&self) -> &[[f64; 3]] {
        &self.body_positions
    }

    /// Returns post-forward Cartesian body orientations in MuJoCo quaternion order.
    #[must_use]
    pub fn body_orientations(&self) -> &[[f64; 4]] {
        &self.body_orientations
    }
}

/// One completed fixed-step result.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneStep {
    /// Starting completed boundary.
    pub start_boundary: u64,
    /// Ending completed boundary.
    pub end_boundary: u64,
    /// Copied post-step native state.
    pub state: StateSnapshot,
}

/// An owned, non-authoritative MuJoCo data workspace.
///
/// Workspaces are intentionally thread-affine even though the underlying
/// binding can mark some data/model combinations `Send`.
/// This prevents native mutable data from being moved into a service worker or
/// renderer without an explicit owner boundary.
pub struct Workspace {
    model: Model,
    data: MjData<std::sync::Arc<mujoco_rs::wrappers::MjModel>>,
    _thread_affine: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Workspace")
            .field("model", &self.model.identity())
            .field("time_seconds", &self.data.time())
            .finish_non_exhaustive()
    }
}

impl Workspace {
    /// Allocates a data workspace for `model` and performs initial forward evaluation.
    pub fn new(model: &Model) -> Result<Self, WorkspaceError> {
        let model_for_data = model.inner_arc();
        let mut data = MjData::try_new(model_for_data)
            .map_err(|error| WorkspaceError::Allocation(error.to_string()))?;
        data.forward();
        let workspace = Self {
            model: model.clone(),
            data,
            _thread_affine: PhantomData,
        };
        workspace.ensure_finite_state()?;
        Ok(workspace)
    }

    /// Returns the immutable model paired with this workspace.
    #[must_use]
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Returns the current copied workspace state.
    pub fn snapshot(&self) -> Result<StateSnapshot, WorkspaceError> {
        self.ensure_finite_state()?;
        let time_seconds = self.data.time();
        Ok(StateSnapshot {
            boundary: 0,
            time_seconds,
            qpos: self.data.qpos().to_vec().into_boxed_slice(),
            qvel: self.data.qvel().to_vec().into_boxed_slice(),
            controls: self.data.ctrl().to_vec().into_boxed_slice(),
            sensor_data: self.data.sensordata().to_vec().into_boxed_slice(),
            body_positions: self.data.xpos().to_vec().into_boxed_slice(),
            body_orientations: self.data.xquat().to_vec().into_boxed_slice(),
        })
    }

    /// Resets the native data and performs forward evaluation without integrating time.
    pub fn reset(&mut self) -> Result<(), WorkspaceError> {
        self.data.reset();
        self.data.forward();
        self.ensure_finite_state()
    }

    /// Performs forward kinematics/sensor evaluation without integrating time.
    pub fn forward(&mut self) -> Result<(), WorkspaceError> {
        self.data.forward();
        self.ensure_finite_state()
    }

    /// Replaces the generalized positions used by this private workspace.
    pub fn set_qpos(&mut self, values: &[f64]) -> Result<(), WorkspaceError> {
        let expected = self.model.counts().qpos;
        validate_state_values("qpos", values, expected)?;
        self.data.qpos_mut().copy_from_slice(values);
        Ok(())
    }

    /// Replaces the generalized velocities used by this private workspace.
    pub fn set_qvel(&mut self, values: &[f64]) -> Result<(), WorkspaceError> {
        let expected = self.model.counts().qvel;
        validate_state_values("qvel", values, expected)?;
        self.data.qvel_mut().copy_from_slice(values);
        Ok(())
    }

    /// Replaces scalar controls used by this private workspace.
    pub fn set_controls(&mut self, values: &[f64]) -> Result<(), WorkspaceError> {
        let expected = self.model.counts().controls;
        validate_state_values("controls", values, expected)?;
        self.data.ctrl_mut().copy_from_slice(values);
        Ok(())
    }

    /// Steps this private workspace once.
    pub fn step(&mut self) -> Result<(), WorkspaceError> {
        self.data.step();
        self.ensure_finite_state()
    }

    fn ensure_finite_state(&self) -> Result<(), WorkspaceError> {
        let time_seconds = self.data.time();
        if !time_seconds.is_finite() {
            return Err(WorkspaceError::Native {
                operation: "read time",
                message: format!("non-finite native time {time_seconds}"),
            });
        }
        for (name, values) in [
            ("qpos", self.data.qpos()),
            ("qvel", self.data.qvel()),
            ("controls", self.data.ctrl()),
            ("sensor_data", self.data.sensordata()),
        ] {
            validate_finite_slice(name, values)?;
        }
        for (index, vector) in self.data.xpos().iter().enumerate() {
            for (component, value) in vector.iter().copied().enumerate() {
                if !value.is_finite() {
                    return Err(WorkspaceError::NonFinite {
                        name: "body_positions",
                        index: index * 3 + component,
                        value,
                    });
                }
            }
        }
        for (index, vector) in self.data.xquat().iter().enumerate() {
            for (component, value) in vector.iter().copied().enumerate() {
                if !value.is_finite() {
                    return Err(WorkspaceError::NonFinite {
                        name: "body_orientations",
                        index: index * 4 + component,
                        value,
                    });
                }
            }
        }
        Ok(())
    }
}

fn validate_finite_slice(name: &'static str, values: &[f64]) -> Result<(), WorkspaceError> {
    for (index, value) in values.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(WorkspaceError::NonFinite { name, index, value });
        }
    }
    Ok(())
}

fn validate_state_values(
    name: &'static str,
    values: &[f64],
    expected: usize,
) -> Result<(), WorkspaceError> {
    if values.len() != expected {
        return Err(WorkspaceError::Length {
            name,
            actual: values.len(),
            expected,
        });
    }
    for (index, value) in values.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(WorkspaceError::NonFinite { name, index, value });
        }
    }
    Ok(())
}

/// The authoritative scene owner for one fixed native model.
///
/// This type is thread-affine and deliberately does not expose `mjData`.
/// All native mutation occurs through its bounded methods, making the scene's
/// logical boundary and native progress observable without serializing private
/// native memory.
pub struct Scene {
    workspace: Workspace,
    quantum: PhysicsQuantum,
    boundary: u64,
    phase: ScenePhase,
    _thread_affine: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for Scene {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Scene")
            .field("model", &self.workspace.model().identity())
            .field("quantum", &self.quantum)
            .field("boundary", &self.boundary)
            .field("phase", &self.phase)
            .finish()
    }
}

impl Scene {
    /// Creates a paused scene using the model's source-authored native timestep.
    pub fn new(model: Model) -> Result<Self, SceneError> {
        let quantum = PhysicsQuantum::from_seconds(model.timestep())?;
        let workspace = Workspace::new(&model)?;
        Ok(Self {
            workspace,
            quantum,
            boundary: 0,
            phase: ScenePhase::Paused,
            _thread_affine: PhantomData,
        })
    }

    /// Returns the immutable model used by the scene.
    #[must_use]
    pub fn model(&self) -> &Model {
        self.workspace.model()
    }

    /// Returns the source-authored fixed quantum.
    #[must_use]
    pub const fn quantum(&self) -> PhysicsQuantum {
        self.quantum
    }

    /// Returns the scene lifecycle phase.
    #[must_use]
    pub const fn phase(&self) -> ScenePhase {
        self.phase
    }

    /// Returns the last completed native boundary.
    #[must_use]
    pub const fn boundary(&self) -> u64 {
        self.boundary
    }

    /// Returns a copied state snapshot at the current boundary.
    pub fn snapshot(&self) -> Result<StateSnapshot, SceneError> {
        let mut snapshot = self.workspace.snapshot()?;
        snapshot.boundary = self.boundary;
        Ok(snapshot)
    }

    /// Selects one scalar control for the next native transition.
    ///
    /// Controls are validated as finite values and against the model's native
    /// finite control range before any data mutation occurs.
    pub fn set_control(&mut self, index: usize, value: f64) -> Result<(), SceneError> {
        if self.phase == ScenePhase::Failed {
            return Err(SceneError::Failed);
        }
        let mut controls = self.workspace.snapshot()?.controls.into_vec();
        if index >= controls.len() {
            return Err(SceneError::ControlIndex {
                index,
                length: controls.len(),
            });
        }
        validate_control(&self.workspace, index, value)?;
        controls[index] = value;
        self.workspace.set_controls(&controls)?;
        Ok(())
    }

    /// Selects all scalar controls for the next native transition atomically.
    pub fn set_controls(&mut self, values: &[f64]) -> Result<(), SceneError> {
        if self.phase == ScenePhase::Failed {
            return Err(SceneError::Failed);
        }
        let expected = self.model().counts().controls;
        if values.len() != expected {
            return Err(SceneError::ControlLength {
                actual: values.len(),
                expected,
            });
        }
        for (index, value) in values.iter().copied().enumerate() {
            validate_control(&self.workspace, index, value)?;
        }
        self.workspace.set_controls(values)?;
        Ok(())
    }

    /// Performs exactly one native transition.
    pub fn step(&mut self) -> Result<SceneStep, SceneError> {
        self.advance(1)
    }

    /// Performs one native transition using a complete validated control cut.
    ///
    /// This is crate-visible so the controlled provider coordinator can place
    /// its admission boundary immediately before native mutation while direct
    /// callers continue to use [`Scene::step`] or [`Scene::advance`].
    pub(crate) fn integrate_controls(&mut self, controls: &[f64]) -> Result<SceneStep, SceneError> {
        if self.phase == ScenePhase::Failed {
            return Err(SceneError::Failed);
        }
        let start_boundary = self.boundary;
        let end_boundary = start_boundary
            .checked_add(1)
            .ok_or(SceneError::BoundaryOverflow {
                start: start_boundary,
                count: 1,
            })?;
        if controls.len() != self.model().counts().controls {
            return Err(SceneError::ControlLength {
                actual: controls.len(),
                expected: self.model().counts().controls,
            });
        }
        for (index, value) in controls.iter().copied().enumerate() {
            validate_control(&self.workspace, index, value)?;
        }

        self.phase = ScenePhase::Running;
        if let Err(error) = self.workspace.set_controls(controls) {
            self.phase = ScenePhase::Failed;
            return Err(SceneError::Workspace(error));
        }
        if let Err(error) = self.workspace.step() {
            self.phase = ScenePhase::Failed;
            return Err(SceneError::Workspace(error));
        }
        self.boundary = end_boundary;
        let native_time = match self.workspace.snapshot() {
            Ok(snapshot) => snapshot.time_seconds,
            Err(error) => {
                self.phase = ScenePhase::Failed;
                return Err(SceneError::Workspace(error));
            }
        };
        if !native_time.is_finite() {
            self.phase = ScenePhase::Failed;
            return Err(SceneError::NonFiniteTime(native_time));
        }
        let expected = self.boundary as f64 * self.quantum.as_seconds();
        if !time_matches(native_time, expected) {
            self.phase = ScenePhase::Failed;
            return Err(SceneError::TimeMismatch {
                actual: native_time,
                expected,
            });
        }
        self.phase = ScenePhase::Paused;
        let mut state = match self.workspace.snapshot() {
            Ok(state) => state,
            Err(error) => {
                self.phase = ScenePhase::Failed;
                return Err(SceneError::Workspace(error));
            }
        };
        state.boundary = self.boundary;
        Ok(SceneStep {
            start_boundary,
            end_boundary,
            state,
        })
    }

    /// Performs exactly `count` native transitions, including every intermediate
    /// state update and boundary check.
    pub fn advance(&mut self, count: u64) -> Result<SceneStep, SceneError> {
        if count == 0 {
            return Err(SceneError::ZeroAdvance);
        }
        if self.phase == ScenePhase::Failed {
            return Err(SceneError::Failed);
        }
        let start_boundary = self.boundary;
        let end_boundary =
            start_boundary
                .checked_add(count)
                .ok_or(SceneError::BoundaryOverflow {
                    start: start_boundary,
                    count,
                })?;
        self.phase = ScenePhase::Running;
        for _ in 0..count {
            let controls = match self.workspace.snapshot() {
                Ok(snapshot) => snapshot.controls().to_vec(),
                Err(error) => {
                    self.phase = ScenePhase::Failed;
                    return Err(SceneError::Workspace(error));
                }
            };
            if let Err(error) = self.integrate_controls(&controls) {
                self.phase = ScenePhase::Failed;
                return Err(error);
            }
        }
        self.phase = ScenePhase::Paused;
        let mut state = match self.workspace.snapshot() {
            Ok(state) => state,
            Err(error) => {
                self.phase = ScenePhase::Failed;
                return Err(SceneError::Workspace(error));
            }
        };
        state.boundary = end_boundary;
        Ok(SceneStep {
            start_boundary,
            end_boundary,
            state,
        })
    }

    /// Resets the same compiled model to boundary zero without changing model identity.
    pub fn reset(&mut self) -> Result<StateSnapshot, SceneError> {
        if self.phase == ScenePhase::Failed {
            return Err(SceneError::Failed);
        }
        if let Err(error) = self.workspace.reset() {
            self.phase = ScenePhase::Failed;
            return Err(SceneError::Workspace(error));
        }
        self.boundary = 0;
        self.phase = ScenePhase::Paused;
        match self.snapshot() {
            Ok(snapshot) => Ok(snapshot),
            Err(error) => {
                self.phase = ScenePhase::Failed;
                Err(error)
            }
        }
    }
}

fn validate_control(workspace: &Workspace, index: usize, value: f64) -> Result<(), SceneError> {
    if !value.is_finite() {
        return Err(SceneError::NonFiniteControl { index, value });
    }
    if let Some([lower, upper]) = workspace.model().control_range(index)?
        && (value < lower || value > upper)
    {
        return Err(SceneError::ControlOutOfRange {
            index,
            value,
            lower,
            upper,
        });
    }
    Ok(())
}

fn time_matches(actual: f64, expected: f64) -> bool {
    if !actual.is_finite() || !expected.is_finite() {
        return false;
    }
    let scale = actual.abs().max(expected.abs()).max(1.0);
    (actual - expected).abs() <= TIME_TOLERANCE.max(TIME_TOLERANCE * scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClosedModel, Resource};

    #[cfg(feature = "native")]
    const FIXTURE: &str = r#"
        <mujoco model="fixed-step-fixture">
          <option timestep="0.01" gravity="0 0 -9.81"/>
          <worldbody>
            <geom name="floor" type="plane" size="2 2 0.1"/>
            <body name="arm" pos="0 0 1">
              <joint name="hinge" type="hinge" axis="0 1 0"/>
              <geom name="arm_geom" type="capsule" fromto="0 0 0 0 0 0.5" size="0.05" mass="1"/>
              <site name="tip" pos="0 0 0.5" size="0.01"/>
            </body>
          </worldbody>
          <actuator>
            <motor name="hinge_motor" joint="hinge" ctrlrange="-1 1" ctrllimited="true"/>
          </actuator>
          <sensor>
            <framepos name="tip_position" objtype="site" objname="tip"/>
          </sensor>
        </mujoco>
    "#;

    #[test]
    fn invalid_quantum_is_rejected() {
        assert!(PhysicsQuantum::from_seconds(0.0).is_err());
        assert!(PhysicsQuantum::from_seconds(f64::NAN).is_err());
        assert!(PhysicsQuantum::from_seconds(f64::INFINITY).is_err());
    }

    #[test]
    fn time_tolerance_is_relative_to_the_native_clock() {
        assert!(time_matches(1.0, 1.0 + 1.0e-10));
        assert!(!time_matches(1.0, 1.1));
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_model_resolves_relative_includes_only_from_the_closed_vfs() {
        let artifact = ClosedModel::new(
            "model.xml",
            [
                Resource::new(
                    "model.xml",
                    br#"<mujoco><include file="body.xml"/></mujoco>"#
                        .to_vec(),
                )
                .unwrap(),
                Resource::new(
                    "body.xml",
                    br#"<mujoco><worldbody><body name="included"><geom type="sphere" size="0.1"/></body></worldbody></mujoco>"#.to_vec(),
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let model = Model::from_closed(artifact).unwrap();
        assert_eq!(model.counts().bodies, 2);
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_model_is_read_only_and_scene_steps_from_zero() {
        let model = Model::from_xml(FIXTURE).unwrap();
        assert_eq!(Model::native_version(), "3.12.0");
        assert_eq!(model.timestep(), 0.01);
        assert_eq!(model.counts().bodies, 2);

        let body = model.body("arm").unwrap().unwrap();
        let body_info = model.body_info(body).unwrap();
        assert_eq!(body_info.position, [0.0, 0.0, 1.0]);
        let joint = model.joint("hinge").unwrap().unwrap();
        assert_eq!(
            model.joint_info(joint).unwrap().kind,
            crate::JointKind::Hinge
        );
        let actuator = model.actuator("hinge_motor").unwrap().unwrap();
        assert_eq!(
            model.actuator_info(actuator).unwrap().control_range,
            Some([-1.0, 1.0])
        );
        let sensor = model.sensor("tip_position").unwrap().unwrap();
        assert_eq!(model.sensor_info(sensor).unwrap().dimension, 3);

        let identity = model.identity();
        let mut scene = Scene::new(model).unwrap();
        assert_eq!(scene.phase(), ScenePhase::Paused);
        assert_eq!(scene.boundary(), 0);
        assert_eq!(scene.snapshot().unwrap().time_seconds(), 0.0);

        scene.set_control(0, 0.5).unwrap();
        let step = scene.advance(3).unwrap();
        assert_eq!(step.start_boundary, 0);
        assert_eq!(step.end_boundary, 3);
        assert_eq!(step.state.boundary(), 3);
        assert_eq!(step.state.time_seconds(), 0.03);
        assert_eq!(scene.model().identity(), identity);
        assert_eq!(scene.phase(), ScenePhase::Paused);

        let reset = scene.reset().unwrap();
        assert_eq!(reset.boundary(), 0);
        assert_eq!(reset.time_seconds(), 0.0);
    }

    #[cfg(feature = "native")]
    #[test]
    fn workspace_accepts_measured_state_without_scene_authority() {
        let model = Model::from_xml(FIXTURE).unwrap();
        let mut workspace = Workspace::new(&model).unwrap();
        workspace.set_qpos(&[0.25]).unwrap();
        workspace.set_qvel(&[0.0]).unwrap();
        workspace.forward().unwrap();
        let state = workspace.snapshot().unwrap();
        assert_eq!(state.qpos(), &[0.25]);
        assert_eq!(state.qvel(), &[0.0]);
        assert_eq!(state.boundary(), 0);
    }

    #[cfg(feature = "native")]
    #[test]
    fn scene_rejects_invalid_controls_before_mutating_data() {
        let model = Model::from_xml(FIXTURE).unwrap();
        let mut scene = Scene::new(model).unwrap();
        assert!(matches!(
            scene.set_controls(&[]),
            Err(SceneError::ControlLength { .. })
        ));
        assert!(matches!(
            scene.set_control(0, 2.0),
            Err(SceneError::ControlOutOfRange { .. })
        ));
        assert!(matches!(
            scene.set_control(0, f64::NAN),
            Err(SceneError::NonFiniteControl { .. })
        ));
        assert_eq!(scene.snapshot().unwrap().controls(), &[0.0]);
    }
}
