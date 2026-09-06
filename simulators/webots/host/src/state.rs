//! Deterministic Webots host state transitions.
//!
//! The public world-session projection is owned by `phoxal`.
//! This module owns only Webots observations, validation, and the native directives from which the
//! host updates that projection.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use phoxal::identity::{ExecutionId, ProducerId};
use phoxal::model::world::WorldProgress;
use phoxal::version::FrameworkVersion;

use phoxal_simulator_webots_shared::protocol::{
    ControllerEvent, ControllerFault, ControllerRole, HostDirective, NativeMotion,
    NativeProgressObservation, ObservedNativeMode,
};

/// The internal lifecycle of the native world.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeWorldLifecycle {
    Starting,
    Ready {
        requested: NativeMotion,
        observed: NativeMotion,
    },
    Stopping,
    Failed(NativeWorldFailure),
}

/// Why shared native authority is no longer trustworthy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeWorldFailure {
    DuplicateWorldController,
    DuplicateRobot {
        execution: String,
    },
    IncompatibleController {
        expected: FrameworkVersion,
        observed: FrameworkVersion,
    },
    InvalidTimeStep,
    InvalidProgress {
        expected_step: u64,
        expected_elapsed_ns: u64,
        observed: NativeProgressObservation,
    },
    UnsupportedMode(ObservedNativeMode),
    WorldControllerLost,
    RobotControllerLost {
        execution: String,
    },
    Controller(ControllerFault),
    Protocol(String),
}

/// A cooperative per-member fault that leaves shared world authority intact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeRobotFailure {
    Controller(ControllerFault),
    SupervisorLost,
}

/// A validated Webots host state machine.
#[derive(Clone, Debug)]
pub struct NativeWorldState {
    lifecycle: NativeWorldLifecycle,
    time_step_ns: Option<u64>,
    progress: NativeProgressObservation,
    world_controller: bool,
    world_stopped: bool,
    world_last_seen: Option<Instant>,
    robots: BTreeMap<String, NativeRobotState>,
    robot_failures: BTreeMap<String, NativeRobotFailure>,
    robot_last_seen: BTreeMap<String, Instant>,
    boundary: Option<BoundaryLatch>,
}

#[derive(Clone, Debug)]
struct BoundaryLatch {
    progress: WorldProgress,
    completed_motion: NativeMotion,
    next_motion: NativeMotion,
    expected: BTreeSet<BoundaryRole>,
    arrivals: BTreeSet<BoundaryRole>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum BoundaryRole {
    World,
    Robot(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum NativeRobotState {
    Connected,
    Ready {
        controller: ProducerId,
        active_revision: Option<u64>,
        observed: NativeMotion,
    },
    Faulted {
        controller: ProducerId,
        failure: NativeRobotFailure,
    },
    Parked {
        controller: ProducerId,
        failure: Option<NativeRobotFailure>,
    },
    Stopped,
    Released,
}

impl Default for NativeWorldState {
    fn default() -> Self {
        Self {
            lifecycle: NativeWorldLifecycle::Starting,
            time_step_ns: None,
            progress: NativeProgressObservation {
                completed_step: 0,
                elapsed_ns: 0,
                mode: ObservedNativeMode::Paused,
            },
            world_controller: false,
            world_stopped: false,
            world_last_seen: None,
            robots: BTreeMap::new(),
            robot_failures: BTreeMap::new(),
            robot_last_seen: BTreeMap::new(),
            boundary: None,
        }
    }
}

impl NativeWorldState {
    #[must_use]
    pub const fn lifecycle(&self) -> &NativeWorldLifecycle {
        &self.lifecycle
    }

    #[must_use]
    pub const fn progress(&self) -> NativeProgressObservation {
        self.progress
    }

    #[must_use]
    pub fn directive(&self) -> HostDirective {
        match &self.lifecycle {
            NativeWorldLifecycle::Starting => HostDirective::Park,
            NativeWorldLifecycle::Ready { requested, .. } => {
                HostDirective::Continue { motion: *requested }
            }
            NativeWorldLifecycle::Stopping => HostDirective::Stop {
                reason: "the world session is stopping".to_owned(),
            },
            NativeWorldLifecycle::Failed(reason) => HostDirective::Stop {
                reason: format!("the native world failed: {reason:?}"),
            },
        }
    }

    /// Admit one exact-train native controller.
    pub fn admit(
        &mut self,
        framework: FrameworkVersion,
        role: ControllerRole,
    ) -> Result<HostDirective, NativeWorldFailure> {
        if framework != FrameworkVersion::CURRENT {
            return Err(NativeWorldFailure::IncompatibleController {
                expected: FrameworkVersion::CURRENT,
                observed: framework,
            });
        }
        match role {
            ControllerRole::World if self.world_controller => {
                Err(NativeWorldFailure::DuplicateWorldController)
            }
            ControllerRole::World => {
                self.world_controller = true;
                self.world_last_seen = Some(Instant::now());
                Ok(self.directive())
            }
            ControllerRole::Robot { execution } => {
                let execution = execution_key(execution);
                if self.robots.contains_key(&execution)
                    || self.robot_failures.contains_key(&execution)
                {
                    return Err(NativeWorldFailure::DuplicateRobot { execution });
                }
                self.robots
                    .insert(execution.clone(), NativeRobotState::Connected);
                self.robot_last_seen.insert(execution, Instant::now());
                Ok(self.directive())
            }
        }
    }

    /// Apply one observation from a controller.
    pub fn observe(
        &mut self,
        role: ControllerRole,
        event: ControllerEvent,
    ) -> Result<HostDirective, NativeWorldFailure> {
        self.touch(role);
        match (role, event) {
            (ControllerRole::World, ControllerEvent::WorldReady { time_step_ns, mode }) => {
                if time_step_ns == 0 {
                    return self.fail(NativeWorldFailure::InvalidTimeStep);
                }
                if mode != ObservedNativeMode::Paused {
                    return self.fail(mode_failure(mode));
                }
                self.time_step_ns = Some(time_step_ns);
                self.world_stopped = false;
                self.lifecycle = NativeWorldLifecycle::Ready {
                    requested: NativeMotion::Paused,
                    observed: NativeMotion::Paused,
                };
            }
            (ControllerRole::World, ControllerEvent::WorldMode { mode }) => {
                self.observe_mode(mode)?;
            }
            (ControllerRole::World, ControllerEvent::WorldProgress(progress)) => {
                self.observe_progress(progress)?;
                let progress = self.public_progress()?;
                return self.observe_completed_boundary(
                    BoundaryRole::World,
                    progress,
                    NativeMotion::RealTime,
                );
            }
            (ControllerRole::World, ControllerEvent::Fault(fault)) => {
                return self.fail(match fault {
                    ControllerFault::UnsupportedMode { observed } => {
                        NativeWorldFailure::UnsupportedMode(observed)
                    }
                    other => NativeWorldFailure::Controller(other),
                });
            }
            (ControllerRole::World, ControllerEvent::Stopped) => {
                if !matches!(
                    self.lifecycle,
                    NativeWorldLifecycle::Stopping | NativeWorldLifecycle::Failed(_)
                ) {
                    return self.fail(NativeWorldFailure::WorldControllerLost);
                }
                self.world_stopped = true;
            }
            (ControllerRole::World, ControllerEvent::Heartbeat) => {
                if matches!(
                    self.lifecycle,
                    NativeWorldLifecycle::Ready {
                        observed: NativeMotion::Paused,
                        ..
                    }
                ) {
                    let progress = self.public_progress()?;
                    return self.observe_completed_boundary(
                        BoundaryRole::World,
                        progress,
                        NativeMotion::Paused,
                    );
                }
            }
            (
                ControllerRole::World,
                ControllerEvent::MutationCompleted { .. } | ControllerEvent::RobotImported { .. },
            ) => {}
            (ControllerRole::Robot { execution }, ControllerEvent::RobotReady { controller }) => {
                let execution = execution_key(execution);
                if !matches!(
                    self.robots.get(&execution),
                    Some(NativeRobotState::Connected)
                ) {
                    return self.fail(NativeWorldFailure::Protocol(format!(
                        "robot {execution} reported Ready outside its admitted connection"
                    )));
                }
                self.robots.insert(
                    execution,
                    NativeRobotState::Ready {
                        controller,
                        active_revision: None,
                        observed: NativeMotion::Paused,
                    },
                );
            }
            (ControllerRole::Robot { execution }, ControllerEvent::RobotActive { revision }) => {
                let execution = execution_key(execution);
                let Some(NativeRobotState::Ready {
                    controller,
                    active_revision,
                    observed,
                }) = self.robots.get(&execution)
                else {
                    return self.fail(NativeWorldFailure::Protocol(format!(
                        "robot {execution} acknowledged Active before Ready"
                    )));
                };
                if active_revision.is_some_and(|current| revision < current) {
                    return self.fail(NativeWorldFailure::Protocol(format!(
                        "robot {execution} regressed Active revision"
                    )));
                }
                self.robots.insert(
                    execution,
                    NativeRobotState::Ready {
                        controller: *controller,
                        active_revision: Some(revision),
                        observed: *observed,
                    },
                );
            }
            (
                ControllerRole::Robot { execution },
                ControllerEvent::RobotBoundary { progress, motion },
            ) => {
                let execution = execution_key(execution);
                match self.robots.get_mut(&execution) {
                    Some(NativeRobotState::Ready { observed, .. }) => *observed = motion,
                    Some(NativeRobotState::Faulted { .. }) => {}
                    _ => {
                        return self.fail(NativeWorldFailure::Protocol(format!(
                            "robot {execution} reported a boundary before Ready"
                        )));
                    }
                }
                return self.observe_completed_boundary(
                    BoundaryRole::Robot(execution),
                    progress,
                    motion,
                );
            }
            (ControllerRole::Robot { execution }, ControllerEvent::RobotParked) => {
                let execution = execution_key(execution);
                let (controller, failure) = match self.robots.get(&execution) {
                    Some(NativeRobotState::Ready { controller, .. }) => (*controller, None),
                    Some(NativeRobotState::Faulted {
                        controller,
                        failure,
                    }) => (*controller, Some(failure.clone())),
                    Some(NativeRobotState::Parked {
                        controller,
                        failure,
                    }) => (*controller, failure.clone()),
                    Some(
                        NativeRobotState::Connected
                        | NativeRobotState::Stopped
                        | NativeRobotState::Released,
                    )
                    | None => {
                        return self.fail(NativeWorldFailure::Protocol(format!(
                            "robot {execution} parked before reporting its controller identity"
                        )));
                    }
                };
                self.robots.insert(
                    execution,
                    NativeRobotState::Parked {
                        controller,
                        failure,
                    },
                );
            }
            (ControllerRole::Robot { execution }, ControllerEvent::Stopped) => {
                let execution = execution_key(execution);
                let expected = matches!(
                    self.lifecycle,
                    NativeWorldLifecycle::Stopping | NativeWorldLifecycle::Failed(_)
                ) || matches!(
                    self.robots.get(&execution),
                    Some(
                        NativeRobotState::Parked { .. }
                            | NativeRobotState::Stopped
                            | NativeRobotState::Released
                    )
                );
                if !expected {
                    return self.fail(NativeWorldFailure::RobotControllerLost { execution });
                }
                self.robots.insert(execution, NativeRobotState::Stopped);
            }
            (ControllerRole::Robot { .. }, ControllerEvent::ActuationEvidence(_)) => {}
            (ControllerRole::Robot { .. }, ControllerEvent::Heartbeat) => {}
            (ControllerRole::Robot { .. }, ControllerEvent::RobotStopping) => {
                if let NativeWorldLifecycle::Ready { requested, .. } = &mut self.lifecycle {
                    *requested = NativeMotion::Paused;
                }
            }
            (ControllerRole::Robot { execution }, ControllerEvent::Fault(fault)) => {
                let execution = execution_key(execution);
                match self.robots.get(&execution) {
                    Some(NativeRobotState::Ready { controller, .. }) => {
                        let controller = *controller;
                        self.robot_failures.insert(
                            execution.clone(),
                            NativeRobotFailure::Controller(fault.clone()),
                        );
                        self.robots.insert(
                            execution,
                            NativeRobotState::Faulted {
                                controller,
                                failure: NativeRobotFailure::Controller(fault),
                            },
                        );
                        if let NativeWorldLifecycle::Ready { requested, .. } = &mut self.lifecycle {
                            *requested = NativeMotion::Paused;
                        }
                        return Ok(HostDirective::Park);
                    }
                    Some(NativeRobotState::Faulted { .. } | NativeRobotState::Parked { .. }) => {}
                    _ => {
                        return self.fail(NativeWorldFailure::Protocol(format!(
                            "robot {execution} faulted outside an active native barrier"
                        )));
                    }
                }
            }
            (ControllerRole::Robot { execution }, ControllerEvent::RobotSupervisorLost) => {
                let execution = execution_key(execution);
                let Some(NativeRobotState::Ready { controller, .. }) = self.robots.get(&execution)
                else {
                    return self.fail(NativeWorldFailure::Protocol(format!(
                        "robot {execution} lost its supervisor outside an active native barrier"
                    )));
                };
                let controller = *controller;
                self.robot_failures
                    .insert(execution.clone(), NativeRobotFailure::SupervisorLost);
                self.robots.insert(
                    execution,
                    NativeRobotState::Faulted {
                        controller,
                        failure: NativeRobotFailure::SupervisorLost,
                    },
                );
                if let NativeWorldLifecycle::Ready { requested, .. } = &mut self.lifecycle {
                    *requested = NativeMotion::Paused;
                }
                return Ok(HostDirective::Park);
            }
            _ => {
                return self.fail(NativeWorldFailure::Protocol(
                    "controller event does not match its admitted role".to_owned(),
                ));
            }
        }
        Ok(self.directive())
    }

    #[must_use]
    pub fn robot_controller(&self, execution: ExecutionId) -> Option<ProducerId> {
        match self.robots.get(&execution_key(execution)) {
            Some(
                NativeRobotState::Ready { controller, .. }
                | NativeRobotState::Faulted { controller, .. }
                | NativeRobotState::Parked { controller, .. },
            ) => Some(*controller),
            Some(
                NativeRobotState::Connected
                | NativeRobotState::Stopped
                | NativeRobotState::Released,
            )
            | None => None,
        }
    }

    /// Whether any native controller state remains for this execution.
    #[must_use]
    #[cfg(test)]
    pub fn has_robot(&self, execution: ExecutionId) -> bool {
        self.robots.contains_key(&execution_key(execution))
    }

    /// Whether the admitted world controller acknowledged the host terminal directive.
    #[must_use]
    pub const fn world_is_stopped(&self) -> bool {
        self.world_stopped
    }

    /// Whether a world controller joined this native world at any point.
    #[must_use]
    pub const fn has_world_controller(&self) -> bool {
        self.world_controller
    }

    #[must_use]
    pub fn robot_active_revision(&self, execution: ExecutionId) -> Option<u64> {
        match self.robots.get(&execution_key(execution)) {
            Some(NativeRobotState::Ready {
                active_revision, ..
            }) => *active_revision,
            _ => None,
        }
    }

    #[must_use]
    pub fn robot_is_parked(&self, execution: ExecutionId) -> bool {
        matches!(
            self.robots.get(&execution_key(execution)),
            Some(
                NativeRobotState::Parked { .. }
                    | NativeRobotState::Stopped
                    | NativeRobotState::Released
            )
        )
    }

    #[must_use]
    pub fn robot_failure(&self, execution: ExecutionId) -> Option<NativeRobotFailure> {
        let execution = execution_key(execution);
        self.robot_failures
            .get(&execution)
            .cloned()
            .or_else(|| match self.robots.get(&execution) {
                Some(NativeRobotState::Faulted { failure, .. })
                | Some(NativeRobotState::Parked {
                    failure: Some(failure),
                    ..
                }) => Some(failure.clone()),
                _ => None,
            })
    }

    /// Mark controller state released after native removal or pre-commit rollback.
    /// Retain a tombstone only while its controller connection can still close.
    pub fn release_robot(&mut self, execution: ExecutionId) {
        let execution = execution_key(execution);
        if !self.robot_last_seen.contains_key(&execution) {
            self.robots.remove(&execution);
        } else if self.robots.contains_key(&execution) {
            self.robots
                .insert(execution.clone(), NativeRobotState::Released);
        }
        self.robot_failures.remove(&execution);
        self.robot_last_seen.remove(&execution);
        // Release is admitted only while the native world is isolated at a
        // completed paused boundary. A subsequent heartbeat establishes a
        // fresh latch from the remaining synchronized roles.
        self.boundary = None;
    }

    /// Fail a Ready native world when an admitted synchronized controller stops answering.
    ///
    /// The world role is exempt only while it owns the bounded native mutation call. Robot roles
    /// remain monitored because they continue polling outside `wb_robot_step` while paused.
    pub fn enforce_liveness(
        &mut self,
        now: Instant,
        timeout: Duration,
        world_mutation_active: bool,
    ) {
        if !matches!(self.lifecycle, NativeWorldLifecycle::Ready { .. }) {
            return;
        }
        if !world_mutation_active
            && self
                .world_last_seen
                .is_some_and(|last_seen| now.saturating_duration_since(last_seen) > timeout)
        {
            self.lifecycle = NativeWorldLifecycle::Failed(NativeWorldFailure::WorldControllerLost);
            self.boundary = None;
            return;
        }
        let unresponsive = self.robots.iter().find_map(|(execution, state)| {
            matches!(state, NativeRobotState::Ready { .. })
                .then(|| {
                    self.robot_last_seen
                        .get(execution)
                        .is_some_and(|last_seen| {
                            now.saturating_duration_since(*last_seen) > timeout
                        })
                        .then(|| execution.clone())
                })
                .flatten()
        });
        if let Some(execution) = unresponsive {
            self.lifecycle =
                NativeWorldLifecycle::Failed(NativeWorldFailure::RobotControllerLost { execution });
            self.boundary = None;
        }
    }

    /// Whether every admitted Robot has confirmed the requested completed boundary.
    #[must_use]
    pub fn robots_observe_motion(&self, motion: NativeMotion) -> bool {
        self.robots.values().all(|robot| match robot {
            NativeRobotState::Ready { observed, .. } => *observed == motion,
            NativeRobotState::Parked { .. }
            | NativeRobotState::Stopped
            | NativeRobotState::Released => motion == NativeMotion::Paused,
            NativeRobotState::Connected | NativeRobotState::Faulted { .. } => false,
        })
    }

    /// Classify an unexpected private controller disconnect.
    pub fn controller_lost(&mut self, role: ControllerRole) {
        let failure = match role {
            ControllerRole::World
                if matches!(
                    self.lifecycle,
                    NativeWorldLifecycle::Stopping | NativeWorldLifecycle::Failed(_)
                ) =>
            {
                return;
            }
            ControllerRole::World => NativeWorldFailure::WorldControllerLost,
            ControllerRole::Robot { execution } => {
                let execution = execution_key(execution);
                if matches!(
                    self.lifecycle,
                    NativeWorldLifecycle::Stopping | NativeWorldLifecycle::Failed(_)
                ) {
                    self.robots.remove(&execution);
                    self.robot_last_seen.remove(&execution);
                    return;
                }
                match self.robots.get(&execution) {
                    Some(NativeRobotState::Connected) => {
                        // A controller that disconnects before publishing its typed identity has
                        // not joined the synchronized barrier. Let the owning attach transaction
                        // roll back this reservation without failing unrelated members.
                        self.robots.remove(&execution);
                        self.robot_last_seen.remove(&execution);
                        return;
                    }
                    Some(NativeRobotState::Parked { .. } | NativeRobotState::Stopped) => {
                        // The removal worker may not have observed the parking acknowledgement
                        // yet. Preserve it until native removal releases this reservation.
                        self.robot_last_seen.remove(&execution);
                        return;
                    }
                    Some(NativeRobotState::Released) => {
                        self.robots.remove(&execution);
                        self.robot_last_seen.remove(&execution);
                        return;
                    }
                    Some(NativeRobotState::Ready { .. } | NativeRobotState::Faulted { .. })
                    | None => NativeWorldFailure::RobotControllerLost { execution },
                }
            }
        };
        self.lifecycle = NativeWorldLifecycle::Failed(failure);
        self.boundary = None;
    }

    /// Request one of the two supported Live motion states.
    pub fn request_motion(
        &mut self,
        requested: NativeMotion,
    ) -> Result<HostDirective, NativeWorldFailure> {
        let NativeWorldLifecycle::Ready {
            requested: current, ..
        } = &mut self.lifecycle
        else {
            return self.fail(NativeWorldFailure::Protocol(
                "motion can change only while the native world is ready".to_owned(),
            ));
        };
        *current = requested;
        Ok(self.directive())
    }

    pub fn stop(&mut self) -> HostDirective {
        self.lifecycle = NativeWorldLifecycle::Stopping;
        self.directive()
    }

    /// Fail the native world when its private coordination authority is unavailable.
    pub fn protocol_failure(&mut self, detail: String) {
        self.lifecycle = NativeWorldLifecycle::Failed(NativeWorldFailure::Protocol(detail));
    }

    fn observe_mode(&mut self, mode: ObservedNativeMode) -> Result<(), NativeWorldFailure> {
        let observed = native_motion(mode).ok_or_else(|| mode_failure(mode))?;
        let NativeWorldLifecycle::Ready {
            observed: current, ..
        } = &mut self.lifecycle
        else {
            return self.fail(NativeWorldFailure::Protocol(
                "a native mode observation arrived before the world was ready".to_owned(),
            ));
        };
        *current = observed;
        self.progress.mode = mode;
        Ok(())
    }

    fn observe_progress(
        &mut self,
        observed: NativeProgressObservation,
    ) -> Result<(), NativeWorldFailure> {
        if observed.mode != ObservedNativeMode::RealTime {
            return self.fail(mode_failure(observed.mode));
        }
        let Some(time_step_ns) = self.time_step_ns else {
            return self.fail(NativeWorldFailure::Protocol(
                "native progress arrived before the world declared its time step".to_owned(),
            ));
        };
        let expected_step = self.progress.completed_step.checked_add(1).ok_or_else(|| {
            NativeWorldFailure::Protocol("the native step counter exhausted".to_owned())
        })?;
        let expected_elapsed_ns = expected_step.checked_mul(time_step_ns).ok_or_else(|| {
            NativeWorldFailure::Protocol("native elapsed time overflowed".to_owned())
        })?;
        if observed.completed_step != expected_step || observed.elapsed_ns != expected_elapsed_ns {
            return self.fail(NativeWorldFailure::InvalidProgress {
                expected_step,
                expected_elapsed_ns,
                observed,
            });
        }
        self.progress = observed;
        if let NativeWorldLifecycle::Ready {
            observed: motion, ..
        } = &mut self.lifecycle
        {
            *motion = NativeMotion::RealTime;
        }
        Ok(())
    }

    fn public_progress(&mut self) -> Result<WorldProgress, NativeWorldFailure> {
        let Some(time_step_ns) = self.time_step_ns else {
            return self.fail(NativeWorldFailure::Protocol(
                "a completed boundary arrived before the world declared its time step".to_owned(),
            ));
        };
        WorldProgress::at(self.progress.completed_step, time_step_ns).map_err(|error| {
            let failure = NativeWorldFailure::Protocol(format!(
                "the validated native progress could not form WorldProgress: {error}"
            ));
            self.lifecycle = NativeWorldLifecycle::Failed(failure.clone());
            failure
        })
    }

    fn observe_completed_boundary(
        &mut self,
        role: BoundaryRole,
        progress: WorldProgress,
        completed_motion: NativeMotion,
    ) -> Result<HostDirective, NativeWorldFailure> {
        let NativeWorldLifecycle::Ready { requested, .. } = self.lifecycle else {
            return Ok(self.directive());
        };
        let Some(time_step_ns) = self.time_step_ns else {
            return self.fail(NativeWorldFailure::Protocol(
                "a completed boundary arrived before the world declared its time step".to_owned(),
            ));
        };
        if progress.validate(time_step_ns).is_err() {
            return self.fail(self.invalid_progress(progress, completed_motion));
        }

        let expected_roles = self.synchronized_roles();
        if !expected_roles.contains(&role) {
            return self.fail(NativeWorldFailure::Protocol(format!(
                "inactive native role {role:?} reported a synchronized boundary"
            )));
        }

        if let Some(latch) = &self.boundary {
            // A role that already received PAUSE can poll at that unchanged boundary while
            // another native role is still finishing its previous transition's local work.
            if latch.arrivals.contains(&role)
                && latch.progress == progress
                && completed_motion == latch.next_motion
            {
                return Ok(HostDirective::Continue {
                    motion: latch.next_motion,
                });
            }
            if latch.progress != progress || latch.completed_motion != completed_motion {
                return self.fail(NativeWorldFailure::Protocol(format!(
                    "native boundary disagreement: expected {:?} in {:?}, observed {progress:?} in {completed_motion:?}",
                    latch.progress, latch.completed_motion
                )));
            }
        } else {
            let current = self.public_progress()?;
            let allowed = progress == current
                || (completed_motion == NativeMotion::RealTime
                    && current
                        .completed_step()
                        .checked_add(1)
                        .and_then(|step| WorldProgress::at(step, time_step_ns).ok())
                        == Some(progress));
            if !allowed {
                return self.fail(self.invalid_progress(progress, completed_motion));
            }
            self.boundary = Some(BoundaryLatch {
                progress,
                completed_motion,
                next_motion: requested,
                expected: expected_roles.clone(),
                arrivals: BTreeSet::new(),
            });
        }

        let latch = match self.boundary.as_mut() {
            Some(latch) => latch,
            None => {
                return self.fail(NativeWorldFailure::Protocol(
                    "completed boundary latch disappeared".to_owned(),
                ));
            }
        };
        latch.expected = expected_roles;
        latch
            .arrivals
            .retain(|arrived| latch.expected.contains(arrived));
        latch.arrivals.insert(role);
        let next_motion = latch.next_motion;
        if latch.arrivals == latch.expected {
            self.boundary = None;
        }
        Ok(HostDirective::Continue {
            motion: next_motion,
        })
    }

    fn synchronized_roles(&self) -> BTreeSet<BoundaryRole> {
        let mut roles = BTreeSet::new();
        if self.world_controller {
            roles.insert(BoundaryRole::World);
        }
        roles.extend(
            self.robots
                .iter()
                .filter(|(_, state)| {
                    matches!(
                        state,
                        NativeRobotState::Ready { .. } | NativeRobotState::Faulted { .. }
                    )
                })
                .map(|(execution, _)| BoundaryRole::Robot(execution.clone())),
        );
        roles
    }

    fn invalid_progress(
        &self,
        progress: WorldProgress,
        motion: NativeMotion,
    ) -> NativeWorldFailure {
        let time_step_ns = self.time_step_ns.unwrap_or(0);
        let expected_step = if motion == NativeMotion::RealTime {
            self.progress.completed_step.saturating_add(1)
        } else {
            self.progress.completed_step
        };
        NativeWorldFailure::InvalidProgress {
            expected_step,
            expected_elapsed_ns: expected_step.saturating_mul(time_step_ns),
            observed: NativeProgressObservation {
                completed_step: progress.completed_step(),
                elapsed_ns: progress.elapsed_ns(),
                mode: match motion {
                    NativeMotion::Paused => ObservedNativeMode::Paused,
                    NativeMotion::RealTime => ObservedNativeMode::RealTime,
                },
            },
        }
    }

    fn touch(&mut self, role: ControllerRole) {
        match role {
            ControllerRole::World => self.world_last_seen = Some(Instant::now()),
            ControllerRole::Robot { execution } => {
                self.robot_last_seen
                    .insert(execution_key(execution), Instant::now());
            }
        }
    }

    fn fail<T>(&mut self, failure: NativeWorldFailure) -> Result<T, NativeWorldFailure> {
        self.boundary = None;
        self.lifecycle = NativeWorldLifecycle::Failed(failure.clone());
        Err(failure)
    }
}

fn mode_failure(mode: ObservedNativeMode) -> NativeWorldFailure {
    match mode {
        ObservedNativeMode::Run | ObservedNativeMode::Fast => {
            NativeWorldFailure::UnsupportedMode(mode)
        }
        ObservedNativeMode::Paused | ObservedNativeMode::RealTime => NativeWorldFailure::Protocol(
            format!("native mode {mode:?} is invalid at this transition"),
        ),
    }
}

const fn native_motion(mode: ObservedNativeMode) -> Option<NativeMotion> {
    match mode {
        ObservedNativeMode::Paused => Some(NativeMotion::Paused),
        ObservedNativeMode::RealTime => Some(NativeMotion::RealTime),
        ObservedNativeMode::Run | ObservedNativeMode::Fast => None,
    }
}

fn execution_key(execution: ExecutionId) -> String {
    execution.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compatible_patch_other_than_current() -> FrameworkVersion {
        let current = FrameworkVersion::CURRENT;
        let patch = if current.patch() == u16::MAX {
            current.patch() - 1
        } else {
            current.patch() + 1
        };
        FrameworkVersion::new(current.major(), current.minor(), patch)
    }

    fn ready_two_robot_barrier() -> (NativeWorldState, [ControllerRole; 2]) {
        let mut state = NativeWorldState::default();
        state
            .admit(FrameworkVersion::CURRENT, ControllerRole::World)
            .expect("world role");
        state
            .observe(
                ControllerRole::World,
                ControllerEvent::WorldReady {
                    time_step_ns: 12_000_000,
                    mode: ObservedNativeMode::Paused,
                },
            )
            .expect("world ready");
        let executions = [
            ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001)
                .expect("first execution"),
            ExecutionId::try_from(0x2000_0000_0000_0000_0000_0000_0000_0002)
                .expect("second execution"),
        ];
        let controllers = [
            ProducerId::try_from(0x3000_0000_0000_0000_0000_0000_0000_0003)
                .expect("first controller"),
            ProducerId::try_from(0x4000_0000_0000_0000_0000_0000_0000_0004)
                .expect("second controller"),
        ];
        let roles = executions.map(|execution| ControllerRole::Robot { execution });
        for (role, controller) in roles.into_iter().zip(controllers) {
            state
                .admit(FrameworkVersion::CURRENT, role)
                .expect("Robot role");
            state
                .observe(role, ControllerEvent::RobotReady { controller })
                .expect("Robot ready");
        }
        state
            .request_motion(NativeMotion::RealTime)
            .expect("world starts running");
        (state, roles)
    }

    fn robot_boundary(step: u64) -> ControllerEvent {
        ControllerEvent::RobotBoundary {
            progress: WorldProgress::at(step, 12_000_000).expect("boundary progress"),
            motion: NativeMotion::RealTime,
        }
    }

    fn world_boundary(step: u64) -> ControllerEvent {
        ControllerEvent::WorldProgress(NativeProgressObservation {
            completed_step: step,
            elapsed_ns: step * 12_000_000,
            mode: ObservedNativeMode::RealTime,
        })
    }

    fn assert_motion(directive: HostDirective, motion: NativeMotion) {
        assert_eq!(directive, HostDirective::Continue { motion });
    }

    #[test]
    fn native_controller_admission_requires_the_exact_patch_train() {
        let mut state = NativeWorldState::default();
        let observed = compatible_patch_other_than_current();
        assert!(observed.is_compatible_with(FrameworkVersion::CURRENT));

        assert_eq!(
            state
                .admit(observed, ControllerRole::World)
                .expect_err("a compatible but non-exact controller train is rejected"),
            NativeWorldFailure::IncompatibleController {
                expected: FrameworkVersion::CURRENT,
                observed,
            }
        );
        assert_eq!(state.lifecycle(), &NativeWorldLifecycle::Starting);

        state
            .admit(FrameworkVersion::CURRENT, ControllerRole::World)
            .expect("the failed handshake did not consume the world-controller role");
    }

    #[test]
    fn robot_first_boundary_latches_one_next_motion_for_every_role() {
        let (mut state, [first, second]) = ready_two_robot_barrier();
        assert_motion(
            state
                .observe(first, robot_boundary(1))
                .expect("first Robot closes step one"),
            NativeMotion::RealTime,
        );
        state
            .request_motion(NativeMotion::Paused)
            .expect("pause is requested between arrivals");
        assert_motion(
            state
                .observe(ControllerRole::World, world_boundary(1))
                .expect("world closes step one"),
            NativeMotion::RealTime,
        );
        assert_motion(
            state
                .observe(second, robot_boundary(1))
                .expect("second Robot closes step one"),
            NativeMotion::RealTime,
        );

        assert_motion(
            state
                .observe(second, robot_boundary(2))
                .expect("second Robot closes step two first"),
            NativeMotion::Paused,
        );
        assert_motion(
            state
                .observe(ControllerRole::World, world_boundary(2))
                .expect("world closes step two"),
            NativeMotion::Paused,
        );
        assert_motion(
            state
                .observe(first, robot_boundary(2))
                .expect("first Robot closes step two"),
            NativeMotion::Paused,
        );
    }

    #[test]
    fn world_first_boundary_latches_one_next_motion_for_every_role() {
        let (mut state, [first, second]) = ready_two_robot_barrier();
        assert_motion(
            state
                .observe(ControllerRole::World, world_boundary(1))
                .expect("world closes step one first"),
            NativeMotion::RealTime,
        );
        state
            .request_motion(NativeMotion::Paused)
            .expect("pause is requested between arrivals");
        for role in [second, first] {
            assert_motion(
                state
                    .observe(role, robot_boundary(1))
                    .expect("Robot closes step one"),
                NativeMotion::RealTime,
            );
        }
    }

    #[test]
    fn stopped_answering_deadline_exempts_only_the_bounded_world_mutation() {
        let mut state = NativeWorldState::default();
        state
            .admit(FrameworkVersion::CURRENT, ControllerRole::World)
            .expect("world role");
        state
            .observe(
                ControllerRole::World,
                ControllerEvent::WorldReady {
                    time_step_ns: 12_000_000,
                    mode: ObservedNativeMode::Paused,
                },
            )
            .expect("world ready");
        let after_deadline = Instant::now() + Duration::from_secs(31);
        state.enforce_liveness(after_deadline, Duration::from_secs(30), true);
        assert!(matches!(
            state.lifecycle(),
            NativeWorldLifecycle::Ready { .. }
        ));
        state.enforce_liveness(after_deadline, Duration::from_secs(30), false);
        assert_eq!(
            state.lifecycle(),
            &NativeWorldLifecycle::Failed(NativeWorldFailure::WorldControllerLost)
        );
    }

    #[test]
    fn paused_world_keeps_the_robot_stopped_answering_deadline_active() {
        let (mut state, [first, _]) = ready_two_robot_barrier();
        assert!(matches!(
            state.lifecycle(),
            NativeWorldLifecycle::Ready {
                requested: NativeMotion::RealTime,
                observed: NativeMotion::Paused,
            }
        ));
        let after_deadline = Instant::now() + Duration::from_secs(31);
        state.enforce_liveness(after_deadline, Duration::from_secs(30), true);
        assert!(
            matches!(
                state.lifecycle(),
                NativeWorldLifecycle::Failed(NativeWorldFailure::RobotControllerLost { execution })
                    if execution == &match first {
                        ControllerRole::Robot { execution } => execution.to_string(),
                        ControllerRole::World => unreachable!("fixture returned a Robot role"),
                    }
            ),
            "native pause and the world-mutation exemption must not suspend a Robot controller's host-monotonic deadline"
        );
    }

    #[test]
    fn one_exact_quantum_advances_progress_and_a_jump_fails_the_world() {
        let mut state = NativeWorldState::default();
        state
            .admit(FrameworkVersion::CURRENT, ControllerRole::World)
            .expect("the world controller is admitted");
        state
            .observe(
                ControllerRole::World,
                ControllerEvent::WorldReady {
                    time_step_ns: 12_000_000,
                    mode: ObservedNativeMode::Paused,
                },
            )
            .expect("the paused world becomes ready");
        state
            .request_motion(NativeMotion::RealTime)
            .expect("the world can resume");
        state
            .observe(
                ControllerRole::World,
                ControllerEvent::WorldProgress(NativeProgressObservation {
                    completed_step: 1,
                    elapsed_ns: 12_000_000,
                    mode: ObservedNativeMode::RealTime,
                }),
            )
            .expect("one exact quantum is valid");
        assert_eq!(state.progress().completed_step, 1);

        let error = state
            .observe(
                ControllerRole::World,
                ControllerEvent::WorldProgress(NativeProgressObservation {
                    completed_step: 3,
                    elapsed_ns: 36_000_000,
                    mode: ObservedNativeMode::RealTime,
                }),
            )
            .expect_err("skipped progress must fail");
        assert!(matches!(error, NativeWorldFailure::InvalidProgress { .. }));
        assert!(matches!(
            state.lifecycle(),
            NativeWorldLifecycle::Failed(NativeWorldFailure::InvalidProgress { .. })
        ));
    }

    #[test]
    fn rewind_is_refused_under_one_world_instance() {
        let mut state = NativeWorldState::default();
        state
            .admit(FrameworkVersion::CURRENT, ControllerRole::World)
            .expect("the world controller is admitted");
        state
            .observe(
                ControllerRole::World,
                ControllerEvent::WorldReady {
                    time_step_ns: 10,
                    mode: ObservedNativeMode::Paused,
                },
            )
            .expect("the world becomes ready");
        state
            .observe(
                ControllerRole::World,
                ControllerEvent::WorldProgress(NativeProgressObservation {
                    completed_step: 1,
                    elapsed_ns: 10,
                    mode: ObservedNativeMode::RealTime,
                }),
            )
            .expect("the first step is valid");
        assert!(matches!(
            state.observe(
                ControllerRole::World,
                ControllerEvent::WorldProgress(NativeProgressObservation {
                    completed_step: 0,
                    elapsed_ns: 0,
                    mode: ObservedNativeMode::RealTime,
                },)
            ),
            Err(NativeWorldFailure::InvalidProgress { .. })
        ));
    }

    #[test]
    fn fast_and_run_modes_are_typed_world_failures() {
        for mode in [ObservedNativeMode::Run, ObservedNativeMode::Fast] {
            let mut state = NativeWorldState::default();
            state
                .admit(FrameworkVersion::CURRENT, ControllerRole::World)
                .expect("the world controller is admitted");
            assert_eq!(
                state
                    .observe(
                        ControllerRole::World,
                        ControllerEvent::WorldReady {
                            time_step_ns: 12_000_000,
                            mode,
                        }
                    )
                    .expect_err("the mode must fail"),
                NativeWorldFailure::UnsupportedMode(mode)
            );
        }
    }

    #[test]
    fn cooperative_member_fault_is_isolated_but_hard_disconnect_is_world_fatal() {
        let mut state = NativeWorldState::default();
        state
            .admit(FrameworkVersion::CURRENT, ControllerRole::World)
            .expect("the world controller is admitted");
        state
            .observe(
                ControllerRole::World,
                ControllerEvent::WorldReady {
                    time_step_ns: 12_000_000,
                    mode: ObservedNativeMode::Paused,
                },
            )
            .expect("the world becomes ready");
        state
            .request_motion(NativeMotion::RealTime)
            .expect("the world starts running");

        let first = ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001)
            .expect("canonical execution");
        let second = ExecutionId::try_from(0x2000_0000_0000_0000_0000_0000_0000_0002)
            .expect("canonical execution");
        let first_controller = ProducerId::try_from(0x3000_0000_0000_0000_0000_0000_0000_0003)
            .expect("canonical producer");
        let second_controller = ProducerId::try_from(0x4000_0000_0000_0000_0000_0000_0000_0004)
            .expect("canonical producer");
        for (execution, controller) in [(first, first_controller), (second, second_controller)] {
            let role = ControllerRole::Robot { execution };
            state
                .admit(FrameworkVersion::CURRENT, role)
                .expect("the Robot role is admitted");
            state
                .observe(role, ControllerEvent::RobotReady { controller })
                .expect("the Robot becomes ready");
        }
        assert!(state.robots_observe_motion(NativeMotion::Paused));
        assert!(!state.robots_observe_motion(NativeMotion::RealTime));
        for execution in [first, second] {
            state
                .observe(
                    ControllerRole::Robot { execution },
                    ControllerEvent::RobotBoundary {
                        progress: WorldProgress::at(1, 12_000_000).expect("first boundary"),
                        motion: NativeMotion::RealTime,
                    },
                )
                .expect("each Robot confirms the completed running boundary");
        }
        assert!(state.robots_observe_motion(NativeMotion::RealTime));

        let first_role = ControllerRole::Robot { execution: first };
        let directive = state
            .observe(
                first_role,
                ControllerEvent::Fault(ControllerFault::Device {
                    detail: "encoder read failed".to_owned(),
                }),
            )
            .expect("one cooperative Robot fault is isolated");
        assert_eq!(directive, HostDirective::Park);
        state
            .observe(first_role, ControllerEvent::RobotParked)
            .expect("the faulted Robot confirms its boundary");
        state.controller_lost(first_role);
        assert!(matches!(
            state.lifecycle(),
            NativeWorldLifecycle::Ready {
                requested: NativeMotion::Paused,
                ..
            }
        ));
        assert!(matches!(
            state.robot_failure(first),
            Some(NativeRobotFailure::Controller(
                ControllerFault::Device { .. }
            ))
        ));
        assert_eq!(state.robot_controller(second), Some(second_controller));

        state.controller_lost(ControllerRole::Robot { execution: second });
        assert!(matches!(
            state.lifecycle(),
            NativeWorldLifecycle::Failed(NativeWorldFailure::RobotControllerLost { execution })
                if execution == &second.to_string()
        ));
    }

    #[test]
    fn cooperative_fault_finishes_an_already_issued_native_quantum_before_parking() {
        let mut state = NativeWorldState::default();
        let world = ControllerRole::World;
        let execution =
            ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001).expect("execution");
        let robot = ControllerRole::Robot { execution };
        state
            .admit(FrameworkVersion::CURRENT, world)
            .expect("world");
        state
            .observe(
                world,
                ControllerEvent::WorldReady {
                    time_step_ns: 12_000_000,
                    mode: ObservedNativeMode::Paused,
                },
            )
            .expect("ready");
        state
            .admit(FrameworkVersion::CURRENT, robot)
            .expect("robot");
        state
            .observe(
                robot,
                ControllerEvent::RobotReady {
                    controller: ProducerId::try_from(0x2000_0000_0000_0000_0000_0000_0000_0001)
                        .expect("producer"),
                },
            )
            .expect("robot ready");
        state.request_motion(NativeMotion::RealTime).expect("run");
        let first = WorldProgress::at(1, 12_000_000).expect("progress");
        let second = WorldProgress::at(2, 12_000_000).expect("progress");
        let observed = |progress: WorldProgress| {
            ControllerEvent::WorldProgress(NativeProgressObservation {
                completed_step: progress.completed_step(),
                elapsed_ns: progress.elapsed_ns(),
                mode: ObservedNativeMode::RealTime,
            })
        };
        assert_eq!(
            state
                .observe(world, observed(first))
                .expect("world enters next native step"),
            HostDirective::Continue {
                motion: NativeMotion::RealTime
            }
        );
        state
            .observe(
                robot,
                ControllerEvent::Fault(ControllerFault::Device {
                    detail: "capture failed".to_owned(),
                }),
            )
            .expect("fault requests pause");
        assert_eq!(
            state
                .observe(
                    robot,
                    ControllerEvent::RobotBoundary {
                        progress: first,
                        motion: NativeMotion::RealTime
                    }
                )
                .expect("faulted robot remains synchronized"),
            HostDirective::Continue {
                motion: NativeMotion::RealTime
            }
        );
        assert_eq!(
            state
                .observe(world, observed(second))
                .expect("world finishes issued quantum"),
            HostDirective::Continue {
                motion: NativeMotion::Paused
            }
        );
        state
            .observe(
                world,
                ControllerEvent::WorldMode {
                    mode: ObservedNativeMode::Paused,
                },
            )
            .expect("native pause");
        state
            .observe(world, ControllerEvent::Heartbeat)
            .expect("paused poll cannot disagree with a peer completing the same quantum");
        assert_eq!(
            state
                .observe(
                    robot,
                    ControllerEvent::RobotBoundary {
                        progress: second,
                        motion: NativeMotion::RealTime
                    }
                )
                .expect("parked final quantum"),
            HostDirective::Continue {
                motion: NativeMotion::Paused
            }
        );
        state
            .observe(robot, ControllerEvent::RobotParked)
            .expect("parked after common pause");
        assert!(state.robot_is_parked(execution));
        assert!(matches!(
            state.lifecycle(),
            NativeWorldLifecycle::Ready { .. }
        ));
    }

    #[test]
    fn pre_ready_robot_disconnect_remains_an_attachment_rollback() {
        let mut state = NativeWorldState::default();
        state
            .admit(FrameworkVersion::CURRENT, ControllerRole::World)
            .expect("world role");
        state
            .observe(
                ControllerRole::World,
                ControllerEvent::WorldReady {
                    time_step_ns: 12_000_000,
                    mode: ObservedNativeMode::Paused,
                },
            )
            .expect("world ready");
        let execution =
            ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001).expect("execution");
        let role = ControllerRole::Robot { execution };
        state
            .admit(FrameworkVersion::CURRENT, role)
            .expect("pre-ready Robot connection");
        state.controller_lost(role);
        assert!(matches!(
            state.lifecycle(),
            NativeWorldLifecycle::Ready { .. }
        ));
        assert_eq!(state.robot_controller(execution), None);
    }

    #[test]
    fn unsolicited_synchronized_controller_stops_are_world_fatal() {
        let mut state = NativeWorldState::default();
        state
            .admit(FrameworkVersion::CURRENT, ControllerRole::World)
            .expect("world role");
        state
            .observe(
                ControllerRole::World,
                ControllerEvent::WorldReady {
                    time_step_ns: 12_000_000,
                    mode: ObservedNativeMode::Paused,
                },
            )
            .expect("world ready");
        assert_eq!(
            state
                .observe(ControllerRole::World, ControllerEvent::Stopped)
                .expect_err("an unsolicited native world stop is fatal"),
            NativeWorldFailure::WorldControllerLost
        );

        let mut state = NativeWorldState::default();
        state
            .admit(FrameworkVersion::CURRENT, ControllerRole::World)
            .expect("world role");
        state
            .observe(
                ControllerRole::World,
                ControllerEvent::WorldReady {
                    time_step_ns: 12_000_000,
                    mode: ObservedNativeMode::Paused,
                },
            )
            .expect("world ready");
        let execution =
            ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001).expect("execution");
        let role = ControllerRole::Robot { execution };
        state
            .admit(FrameworkVersion::CURRENT, role)
            .expect("Robot role");
        state
            .observe(
                role,
                ControllerEvent::RobotReady {
                    controller: ProducerId::try_from(0x3000_0000_0000_0000_0000_0000_0000_0003)
                        .expect("producer"),
                },
            )
            .expect("Robot ready");
        assert_eq!(
            state
                .observe(role, ControllerEvent::Stopped)
                .expect_err("an unsolicited synchronized Robot stop is fatal"),
            NativeWorldFailure::RobotControllerLost {
                execution: execution.to_string(),
            }
        );
    }

    #[test]
    fn stopped_acknowledgements_require_a_host_terminal_or_parked_role() {
        let (mut state, [first, _]) = ready_two_robot_barrier();
        assert!(!state.world_is_stopped());
        state.stop();
        state
            .observe(first, ControllerEvent::Stopped)
            .expect("a Robot acknowledges the host stop");
        state
            .observe(ControllerRole::World, ControllerEvent::Stopped)
            .expect("the world controller acknowledges the host stop");
        assert!(state.world_is_stopped());
        state.controller_lost(first);
        state.controller_lost(ControllerRole::World);
        assert_eq!(state.lifecycle(), &NativeWorldLifecycle::Stopping);

        let (mut state, [first, _]) = ready_two_robot_barrier();
        state
            .observe(first, ControllerEvent::RobotParked)
            .expect("the host-directed retiring Robot parks first");
        state
            .observe(first, ControllerEvent::Stopped)
            .expect("the parked Robot may acknowledge retirement");
        state.controller_lost(first);
        let ControllerRole::Robot { execution } = first else {
            unreachable!();
        };
        assert!(state.robot_is_parked(execution));
        state.release_robot(execution);
        assert!(!state.has_robot(execution));
        assert!(matches!(
            state.lifecycle(),
            NativeWorldLifecycle::Ready { .. }
        ));
    }
}
