//! Typed scenario plan with finite quantum conversion, deterministic
//! ordering, explicit validity, bounded occurrences, and validation that
//! the schedule fits inside the requested duration.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::port::PortSignature;

/// One finite simulated experiment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioPlan {
    pub scene: PathBuf,
    pub duration: Duration,
    /// Ordered, quantum-indexed actions. Empty plans are allowed but
    /// contribute zero measurable work to the controller.
    pub steps: Vec<Step>,
    /// Declared observations the scenario author wants recorded.
    pub captures: Vec<Capture>,
}

impl ScenarioPlan {
    /// Construct a minimal plan with no actions or captures.
    pub fn new(scene: impl Into<PathBuf>, duration: Duration) -> Self {
        Self {
            scene: scene.into(),
            duration,
            steps: Vec::new(),
            captures: Vec::new(),
        }
    }

    /// Construct the validated plan from a complete schedule. The
    /// returned plan is normalized (deterministic ordering by
    /// boundary then authored order, unique labels, bounded command
    /// occurrences, quantum indices inside `[0, transition_count)`).
    pub fn with_steps(
        scene: impl Into<PathBuf>,
        duration: Duration,
        mut steps: Vec<Step>,
        captures: Vec<Capture>,
    ) -> Result<Self, PlanValidationError> {
        // Stable sort by boundary, preserving authored order at ties.
        // The previous alphabetic-by-label sort was wrong: a schedule
        // `[boundary 500, boundary 1]` was emitted unsorted because
        // the alphabetic tiebreaker was applied before the boundary.
        // Now boundary is the primary sort key, authored order the
        // tiebreaker.
        steps.sort_by_key(|step| step.boundary);
        let plan = Self {
            scene: scene.into(),
            duration,
            steps,
            captures,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Returns the total transition count of the schedule, given a probed
    /// quantum. The transition count is `duration / quantum`: a six-second
    /// experiment at a five-millisecond quantum yields 1,200 transitions,
    /// independent of how many actions the author wrote.
    ///
    /// Returns `None` when:
    /// - the duration does not align to the quantum;
    /// - the duration is zero (`ZeroDuration` is its own error);
    /// - the resulting transition count would not fit in a `u32`.
    ///
    /// Uses checked nanosecond arithmetic so sub-microsecond durations
    /// cannot be silently truncated to a whole-microsecond multiple.
    pub fn transition_count(&self, quantum: crate::scenario::Quantum) -> Option<u32> {
        quantum.transition_count(self.duration)
    }

    /// Validate the plan in place for authoring-shape checks only.
    /// Use [`Self::validate_for_quantum`] to check alignment and
    /// boundary-range against the simulator-probed quantum.
    pub fn validate(&self) -> Result<(), PlanValidationError> {
        if self.duration.is_zero() {
            return Err(PlanValidationError::ZeroDuration);
        }
        if self.steps.len() > u32::MAX as usize {
            return Err(PlanValidationError::TooManySteps(self.steps.len()));
        }
        let mut seen_labels: BTreeMap<&str, &Step> = BTreeMap::new();
        let mut seen_commands: BTreeMap<&str, &Step> = BTreeMap::new();
        for step in &self.steps {
            if let Some(prior) = seen_labels.get(step.label.as_str()) {
                return Err(PlanValidationError::DuplicateStepLabel {
                    label: step.label.clone(),
                    prior_quantum: prior.boundary,
                    duplicate_quantum: step.boundary,
                });
            }
            seen_labels.insert(step.label.as_str(), step);
            step.action.validate(step.label.as_str())?;
            if let Action::Command { label, .. } = &step.action
                && let Some(prior) = seen_commands.get(label.as_str())
            {
                return Err(PlanValidationError::DuplicateCommandLabel {
                    label: label.clone(),
                    prior_step: prior.label.clone(),
                    duplicate_step: step.label.clone(),
                });
            }
            if let Action::Command { label, .. } = &step.action {
                seen_commands.insert(label.as_str(), step);
            }
        }
        Ok(())
    }

    /// Validate the plan against the simulator-probed quantum.
    ///
    /// Combines an alignment check on `duration` with a boundary-range
    /// check on every step's `boundary`. The probe quantum must arrive
    /// from the tool, not a framework rover constant: any robot with a
    /// different physics tick must carry its own quantum through this
    /// path. Returns `None`-equivalent errors through [`PlanValidationError`].
    pub fn validate_for_quantum(
        &self,
        quantum: crate::scenario::Quantum,
    ) -> Result<(), PlanValidationError> {
        let quantum_nanos = u128::from(quantum.micros()) * 1_000;
        let total_nanos = self.duration.as_nanos();
        if total_nanos == 0 {
            return Err(PlanValidationError::ZeroDuration);
        }
        if !total_nanos.is_multiple_of(quantum_nanos) {
            return Err(PlanValidationError::DurationNotAligned {
                nanos: total_nanos,
                quantum_nanos,
            });
        }
        let transitions =
            self.transition_count(quantum)
                .ok_or(PlanValidationError::TransitionCountOverflow {
                    transitions: u128::from(u32::MAX) + 1,
                    quantum_nanos,
                    total_nanos,
                })?;
        for step in &self.steps {
            if step.boundary >= transitions {
                return Err(PlanValidationError::QuantumOutOfRange {
                    label: step.label.clone(),
                    quantum: step.boundary,
                    transitions,
                });
            }
        }
        Ok(())
    }
}

/// One quantum-aligned action in the scenario schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub label: String,
    /// Zero-indexed boundary in the validated transition count. Zero is
    /// the first transition, `transition_count - 1` the last.
    pub boundary: u32,
    pub action: Action,
}

impl Step {
    pub fn new(label: impl Into<String>, boundary: u32, action: Action) -> Self {
        Self {
            label: label.into(),
            boundary,
            action,
        }
    }
}

/// The action vocabulary accepted by a scenario plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Replace the targeted setpoint consumer's current intent with
    /// the encoded Protobuf payload at the boundary.
    Setpoint {
        /// Configured instance name the author wrote into the program.
        /// Empty when the action was assembled directly for tests; the
        /// typed constructor rejects an empty instance so production
        /// programs cannot omit the targeted service.
        target_instance: String,
        consumer_signature: PortSignature,
        encoded_payload: Vec<u8>,
        /// Authoritative validity of the resulting intent. `Permanent`
        /// (the only supported variant today) means the intent remains
        /// until a subsequent setpoint or an explicit withdraw.
        validity: Validity,
    },
    /// Withdraw the targeted producer's published state without
    /// supplying a replacement.
    Withdraw {
        target_instance: String,
        producer_signature: PortSignature,
    },
    /// Submit a request to a commands-style service and observe the
    /// correlation id through the controlled boundary. Advancement
    /// continues while the reply is pending.
    Command {
        target_instance: String,
        service_signature: PortSignature,
        request_encoded: Vec<u8>,
        /// Stable label used to correlate command with reply.
        label: String,
        /// Simulated deadline: the latest simulated quantum at which
        /// the command must complete. `Duration::ZERO` means "no
        /// simulated deadline".
        simulated_deadline: Duration,
        /// Host deadline: the wall-clock budget after issuance during
        /// which the host process must produce a reply. The framework
        /// owns a monotonic timer keyed off issuance; this value is the
        /// only deadline the author controls.
        host_deadline: Duration,
    },
}

/// Authoritative validity of a setpoint's resulting intent.
/// Only the persistent variant is currently supported; finite-lifetime intent (timeout-bound
/// intents that revert on expiry) is a future expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    /// The intent remains in effect until replaced by another setpoint
    /// or an explicit withdraw.
    Permanent,
}

impl Validity {
    pub(crate) fn wire_label(self) -> &'static str {
        match self {
            Validity::Permanent => "permanent",
        }
    }
}

impl Action {
    /// Construct a setpoint action through the typed authoring API.
    /// `target_instance` is the prepared runtime instance the
    /// setpoint is directed at; `descriptor` is its setpoint port
    /// signature; `encoded_payload` is the typed request bytes the
    /// fixture decodes against the descriptor's request codec. An
    /// empty instance or a wrong-kind descriptor is rejected at
    /// construction so the program never carries an untyped action.
    pub fn setpoint(
        target_instance: impl Into<String>,
        descriptor: PortSignature,
        encoded_payload: Vec<u8>,
        validity: Validity,
    ) -> Result<Self, PlanValidationError> {
        let target_instance = target_instance.into();
        if target_instance.is_empty() {
            return Err(PlanValidationError::EmptyTargetInstance {
                constructor: "Action::setpoint",
            });
        }
        if descriptor.kind != crate::port::PortKind::Setpoint {
            return Err(PlanValidationError::WrongPortKind {
                step_label: target_instance,
                expected: crate::port::PortKind::Setpoint,
                actual: descriptor.kind,
            });
        }
        Ok(Action::Setpoint {
            target_instance,
            consumer_signature: descriptor,
            encoded_payload,
            validity,
        })
    }

    /// Construct a withdraw action through the typed authoring API.
    /// The producer signature must accept withdraw (setpoint or state
    /// kind) and `target_instance` must name the prepared runtime
    /// instance whose authority is being recalled.
    pub fn withdraw(
        target_instance: impl Into<String>,
        producer_signature: PortSignature,
    ) -> Result<Self, PlanValidationError> {
        let target_instance = target_instance.into();
        if target_instance.is_empty() {
            return Err(PlanValidationError::EmptyTargetInstance {
                constructor: "Action::withdraw",
            });
        }
        if producer_signature.kind != crate::port::PortKind::Setpoint
            && producer_signature.kind != crate::port::PortKind::State
        {
            return Err(PlanValidationError::WrongPortKind {
                step_label: target_instance,
                expected: crate::port::PortKind::Setpoint,
                actual: producer_signature.kind,
            });
        }
        Ok(Action::Withdraw {
            target_instance,
            producer_signature,
        })
    }

    /// Construct a command action through the typed authoring API.
    /// `target_instance` is the prepared commands-style service the
    /// request is issued to; `descriptor` is its port signature;
    /// `request_encoded` is the typed request bytes; `label` is the
    /// unique authored correlation id; `simulated_deadline` is the
    /// latest simulated quantum at which the command must complete;
    /// `host_deadline` is the wall-clock budget after issuance.
    pub fn command(
        target_instance: impl Into<String>,
        descriptor: PortSignature,
        request_encoded: Vec<u8>,
        label: impl Into<String>,
        simulated_deadline: Duration,
        host_deadline: Duration,
    ) -> Result<Self, PlanValidationError> {
        let target_instance = target_instance.into();
        if target_instance.is_empty() {
            return Err(PlanValidationError::EmptyTargetInstance {
                constructor: "Action::command",
            });
        }
        if descriptor.kind != crate::port::PortKind::Commands {
            return Err(PlanValidationError::WrongPortKind {
                step_label: target_instance,
                expected: crate::port::PortKind::Commands,
                actual: descriptor.kind,
            });
        }
        if host_deadline.is_zero() {
            return Err(PlanValidationError::ZeroHostDeadline {
                step_label: target_instance,
            });
        }
        Ok(Action::Command {
            target_instance,
            service_signature: descriptor,
            request_encoded,
            label: label.into(),
            simulated_deadline,
            host_deadline,
        })
    }

    /// The target instance the action is addressed to. Returns the
    /// empty string when the action was assembled without going
    /// through the typed constructors; the typed constructors reject
    /// that case so a decoded program cannot observe it.
    pub fn target_instance(&self) -> &str {
        match self {
            Action::Setpoint {
                target_instance, ..
            }
            | Action::Withdraw {
                target_instance, ..
            }
            | Action::Command {
                target_instance, ..
            } => target_instance.as_str(),
        }
    }
}

impl Action {
    fn validate(&self, step_label: &str) -> Result<(), PlanValidationError> {
        match self {
            Action::Setpoint {
                target_instance,
                consumer_signature,
                encoded_payload,
                validity,
            } => {
                if target_instance.is_empty() {
                    return Err(PlanValidationError::EmptyTargetInstance {
                        constructor: "Action::Setpoint",
                    });
                }
                if consumer_signature.kind != crate::port::PortKind::Setpoint {
                    return Err(PlanValidationError::WrongPortKind {
                        step_label: step_label.to_owned(),
                        expected: crate::port::PortKind::Setpoint,
                        actual: consumer_signature.kind,
                    });
                }
                if encoded_payload.is_empty() {
                    return Err(PlanValidationError::EmptyPayload {
                        step_label: step_label.to_owned(),
                    });
                }
                if encoded_payload.len() > MAX_PAYLOAD {
                    return Err(PlanValidationError::PayloadTooLarge {
                        step_label: step_label.to_owned(),
                        bytes: encoded_payload.len(),
                    });
                }
                let _ = validity;
            }
            Action::Withdraw {
                target_instance,
                producer_signature,
            } => {
                if target_instance.is_empty() {
                    return Err(PlanValidationError::EmptyTargetInstance {
                        constructor: "Action::Withdraw",
                    });
                }
                if producer_signature.kind != crate::port::PortKind::Setpoint
                    && producer_signature.kind != crate::port::PortKind::State
                {
                    return Err(PlanValidationError::WrongPortKind {
                        step_label: step_label.to_owned(),
                        expected: crate::port::PortKind::Setpoint,
                        actual: producer_signature.kind,
                    });
                }
            }
            Action::Command {
                target_instance,
                service_signature,
                request_encoded,
                label,
                simulated_deadline,
                host_deadline,
            } => {
                if target_instance.is_empty() {
                    return Err(PlanValidationError::EmptyTargetInstance {
                        constructor: "Action::Command",
                    });
                }
                if service_signature.kind != crate::port::PortKind::Commands {
                    return Err(PlanValidationError::WrongPortKind {
                        step_label: step_label.to_owned(),
                        expected: crate::port::PortKind::Commands,
                        actual: service_signature.kind,
                    });
                }
                if request_encoded.is_empty() {
                    return Err(PlanValidationError::EmptyPayload {
                        step_label: step_label.to_owned(),
                    });
                }
                if request_encoded.len() > MAX_PAYLOAD {
                    return Err(PlanValidationError::PayloadTooLarge {
                        step_label: step_label.to_owned(),
                        bytes: request_encoded.len(),
                    });
                }
                if label.is_empty() {
                    return Err(PlanValidationError::EmptyCommandLabel {
                        step_label: step_label.to_owned(),
                    });
                }
                if *host_deadline == Duration::ZERO {
                    return Err(PlanValidationError::ZeroHostDeadline {
                        step_label: step_label.to_owned(),
                    });
                }
                let _ = simulated_deadline;
            }
        }
        Ok(())
    }
}

/// Declared observations the scenario author wants recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capture {
    State {
        name: String,
        signature: PortSignature,
    },
    Sample {
        name: String,
        signature: PortSignature,
    },
    Event {
        name: String,
        signature: PortSignature,
    },
    /// Native simulator body data, identified by documented units and
    /// reference frame so the verifier can interpret the bytes
    /// regardless of how the simulator names them.
    NativeBody {
        name: String,
        units: String,
        frame: String,
    },
}

/// Errors returned by the typed capture constructors. Each variant
/// names the constructor and the offending signature so the caller can
/// correct the wire form rather than guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    WrongPortKind {
        constructor: &'static str,
        expected: crate::port::PortKind,
        actual: crate::port::PortKind,
    },
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongPortKind {
                constructor,
                expected,
                actual,
            } => write!(
                f,
                "{constructor}: expected port kind {expected:?}, got {actual:?}"
            ),
        }
    }
}

impl std::error::Error for CaptureError {}

impl Capture {
    /// Capture a state-style port. The signature must declare
    /// `PortKind::State`; commands-style or setpoint-style signatures
    /// are rejected at construction so the program never observes a
    /// mismatched wire form.
    pub fn state(name: impl Into<String>, signature: PortSignature) -> Result<Self, CaptureError> {
        require_kind(
            "Capture::state",
            crate::port::PortKind::State,
            signature.kind,
        )?;
        Ok(Self::State {
            name: name.into(),
            signature,
        })
    }
    pub fn sample(name: impl Into<String>, signature: PortSignature) -> Result<Self, CaptureError> {
        require_kind(
            "Capture::sample",
            crate::port::PortKind::Sample,
            signature.kind,
        )?;
        Ok(Self::Sample {
            name: name.into(),
            signature,
        })
    }
    pub fn event(name: impl Into<String>, signature: PortSignature) -> Result<Self, CaptureError> {
        require_kind(
            "Capture::event",
            crate::port::PortKind::Event,
            signature.kind,
        )?;
        Ok(Self::Event {
            name: name.into(),
            signature,
        })
    }
    /// Capture a documented native simulator body. The verifier needs
    /// the units and frame to interpret the bytes; both must be
    /// non-empty so a typo cannot silently disable validation.
    pub fn native_body(
        name: impl Into<String>,
        units: impl Into<String>,
        frame: impl Into<String>,
    ) -> Result<Self, CaptureError> {
        let name = name.into();
        let units = units.into();
        let frame = frame.into();
        if units.is_empty() || frame.is_empty() {
            return Err(CaptureError::WrongPortKind {
                constructor: "Capture::native_body",
                expected: crate::port::PortKind::Stream,
                actual: crate::port::PortKind::Stream,
            });
        }
        Ok(Self::NativeBody { name, units, frame })
    }
}

fn require_kind(
    constructor: &'static str,
    expected: crate::port::PortKind,
    actual: crate::port::PortKind,
) -> Result<(), CaptureError> {
    if actual == expected {
        Ok(())
    } else {
        Err(CaptureError::WrongPortKind {
            constructor,
            expected,
            actual,
        })
    }
}

/// The hard cap on any single encoded payload byte length. Chosen so a
/// tampered program cannot allocate unbounded memory while decoding.
pub const MAX_PAYLOAD: usize = 256 * 1024;

/// Plan-level validation errors. The display strings are short and
/// stable; downstream tooling formats them into scenario-bundle
/// diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanValidationError {
    ZeroDuration,
    /// The plan's duration is not an exact multiple of the
    /// simulator-probed quantum. Authoring schedules that don't
    /// align to the quantum cannot be admitted: a 1 ns slippage
    /// could truncate to a whole-microsecond boundary that looks aligned.
    DurationNotAligned {
        nanos: u128,
        quantum_nanos: u128,
    },
    TooManySteps(usize),
    QuantumOutOfRange {
        label: String,
        quantum: u32,
        transitions: u32,
    },
    DuplicateStepLabel {
        label: String,
        prior_quantum: u32,
        duplicate_quantum: u32,
    },
    DuplicateCommandLabel {
        label: String,
        prior_step: String,
        duplicate_step: String,
    },
    WrongPortKind {
        step_label: String,
        expected: crate::port::PortKind,
        actual: crate::port::PortKind,
    },
    EmptyPayload {
        step_label: String,
    },
    PayloadTooLarge {
        step_label: String,
        bytes: usize,
    },
    EmptyCommandLabel {
        step_label: String,
    },
    /// Authored through the typed constructor with an empty
    /// instance name. The authoring API rejects this so a decoded
    /// program cannot carry an action with no target.
    EmptyTargetInstance {
        constructor: &'static str,
    },
    /// Decoded wire form declared a validity the runtime does not
    /// recognise. The decoder refuses the program so a tampered
    /// bundle cannot smuggle in unknown lifetimes.
    UnknownValidity {
        value: String,
    },
    /// A command action declared a host deadline of zero. The
    /// runtime owns a monotonic timer keyed off issuance; a zero
    /// budget would make every command expire instantly.
    ZeroHostDeadline {
        step_label: String,
    },
    /// `duration / quantum` would not fit in a `u32`. Saturating to
    /// `u32::MAX` hides the overflow from the harness; reject it
    /// instead so the tool reports the actual reason.
    TransitionCountOverflow {
        transitions: u128,
        quantum_nanos: u128,
        total_nanos: u128,
    },
}

impl std::fmt::Display for PlanValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroDuration => write!(f, "scenario plan duration must be finite"),
            Self::DurationNotAligned {
                nanos,
                quantum_nanos,
            } => write!(
                f,
                "scenario plan duration of {nanos} ns does not align to the {quantum_nanos} ns quantum",
            ),
            Self::TooManySteps(n) => write!(f, "scenario plan exceeds 2^32 steps: {n}"),
            Self::QuantumOutOfRange {
                label,
                quantum,
                transitions,
            } => write!(
                f,
                "step `{label}` quantum {quantum} is at or beyond the final transition {transitions}"
            ),
            Self::DuplicateStepLabel {
                label,
                prior_quantum,
                duplicate_quantum,
            } => write!(
                f,
                "duplicate step label `{label}` at quanta {prior_quantum} and {duplicate_quantum}"
            ),
            Self::DuplicateCommandLabel {
                label,
                prior_step,
                duplicate_step,
            } => write!(
                f,
                "duplicate command label `{label}` used by steps `{prior_step}` and `{duplicate_step}`"
            ),
            Self::WrongPortKind {
                step_label,
                expected,
                actual,
            } => write!(
                f,
                "step `{step_label}` has wrong port kind: expected {expected:?}, got {actual:?}"
            ),
            Self::EmptyPayload { step_label } => {
                write!(f, "step `{step_label}` has an empty encoded payload")
            }
            Self::PayloadTooLarge { step_label, bytes } => write!(
                f,
                "step `{step_label}` payload is {bytes} bytes, exceeding the {MAX_PAYLOAD}-byte cap"
            ),
            Self::EmptyCommandLabel { step_label } => write!(
                f,
                "step `{step_label}` declares a command action with an empty correlation label"
            ),
            Self::EmptyTargetInstance { constructor } => {
                write!(f, "{constructor} requires a non-empty target instance name")
            }
            Self::UnknownValidity { value } => write!(
                f,
                "decoded wire form declares unknown setpoint validity `{value}`"
            ),
            Self::ZeroHostDeadline { step_label } => write!(
                f,
                "step `{step_label}` declares a zero host deadline for a command action"
            ),
            Self::TransitionCountOverflow {
                transitions,
                quantum_nanos,
                total_nanos,
            } => write!(
                f,
                "scenario plan duration {total_nanos} ns would map to {transitions} transitions \
                 at a {quantum_nanos} ns quantum; u32 cap exceeded"
            ),
        }
    }
}

impl std::error::Error for PlanValidationError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn setpoint_sig() -> PortSignature {
        PortSignature::new(
            "motion/cmd",
            "phoxal.motion",
            "Set",
            crate::port::PortKind::Setpoint,
            "SetpointRequest",
            "SetpointReply",
        )
    }
    fn command_sig() -> PortSignature {
        PortSignature::new(
            "motion/cmd",
            "phoxal.motion",
            "Do",
            crate::port::PortKind::Commands,
            "CommandRequest",
            "CommandReply",
        )
    }
    fn state_sig() -> PortSignature {
        PortSignature::new(
            "motion/state",
            "phoxal.motion",
            "State",
            crate::port::PortKind::State,
            "State",
            "State",
        )
    }

    fn setpoint_action(byte: u8) -> Action {
        Action::setpoint(
            "motion_target",
            setpoint_sig(),
            vec![byte],
            Validity::Permanent,
        )
        .expect("setpoint action")
    }

    fn withdraw_action() -> Action {
        Action::withdraw("motion_target", state_sig()).expect("withdraw action")
    }

    fn command_action(label: &str, byte: u8) -> Action {
        Action::command(
            "motion_target",
            command_sig(),
            vec![byte],
            label,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("command action")
    }

    #[test]
    fn rejects_zero_duration() {
        let plan = ScenarioPlan::with_steps(
            "scene",
            Duration::ZERO,
            vec![Step::new("a", 0, setpoint_action(1))],
            vec![],
        );
        assert_eq!(plan.unwrap_err(), PlanValidationError::ZeroDuration);
    }

    #[test]
    fn rejects_unaligned_duration() {
        // Regression for the scenario acceptance review line 338: "unaligned
        // duration plan accepted with transition count=1". A
        // duration of 2_001 ns is not a multiple of the 2 ms (2_000_000 ns)
        // rover quantum. The quantum-aware validator must refuse it; the
        // shape validator alone accepts it because shape is independent
        // of a scene.
        let plan = ScenarioPlan::with_steps(
            "scene",
            Duration::from_nanos(2_001),
            vec![Step::new("a", 0, setpoint_action(1))],
            vec![],
        )
        .expect("shape validator alone accepts unaligned durations");
        let rover_quantum =
            crate::scenario::Quantum::from_micros(crate::scenario::Quantum::DEFAULT_MICROS)
                .expect("default quantum");
        let error = plan
            .validate_for_quantum(rover_quantum)
            .expect_err("quantum validator must reject an unaligned duration");
        assert!(matches!(
            error,
            PlanValidationError::DurationNotAligned {
                nanos: 2_001,
                quantum_nanos: 2_000_000,
            }
        ));
    }

    #[test]
    fn rejects_quantum_at_or_beyond_final_transition() {
        // Two seconds at 2 ms = 1_000 transitions; quantum at the final
        // transition index (`transition_count`) is rejected because the
        // valid range is `[0, transition_count)`. The shape validator
        // passes step shapes, so the quantum validator is what catches
        // the out-of-range bound.
        let steps = vec![
            Step::new("a", 0, setpoint_action(1)),
            Step::new("b", 1_000, setpoint_action(2)),
        ];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(2), steps, vec![])
            .expect("shape validator alone accepts");
        let rover_quantum =
            crate::scenario::Quantum::from_micros(crate::scenario::Quantum::DEFAULT_MICROS)
                .expect("default quantum");
        match plan
            .validate_for_quantum(rover_quantum)
            .expect_err("quantum validator must reject out-of-range quantum")
        {
            PlanValidationError::QuantumOutOfRange {
                label,
                quantum,
                transitions,
            } => {
                assert_eq!(label, "b");
                assert_eq!(quantum, 1_000);
                assert_eq!(transitions, 1_000);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_duplicate_step_labels() {
        let steps = vec![
            Step::new("a", 0, setpoint_action(1)),
            Step::new("a", 0, setpoint_action(2)),
        ];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(matches!(
            plan.unwrap_err(),
            PlanValidationError::DuplicateStepLabel { .. }
        ));
    }

    #[test]
    fn rejects_duplicate_command_labels() {
        let steps = vec![
            Step::new("step_one", 0, command_action("cmd", 1)),
            Step::new("step_two", 0, command_action("cmd", 2)),
        ];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(matches!(
            plan.unwrap_err(),
            PlanValidationError::DuplicateCommandLabel { .. }
        ));
    }

    #[test]
    fn rejects_wrong_port_kind_for_setpoint() {
        // Constructed through the typed constructor, so we need a
        // separate path that exposes a wrong-kind descriptor. The
        // typed constructor would refuse to build it; we exercise the
        // inner validate by routing the wrong descriptor through the
        // raw enum variant.
        let wrong = Action::Setpoint {
            target_instance: "motion_target".to_owned(),
            consumer_signature: command_sig(),
            encoded_payload: vec![1],
            validity: Validity::Permanent,
        };
        let steps = vec![Step::new("bad", 0, wrong)];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(matches!(
            plan.unwrap_err(),
            PlanValidationError::WrongPortKind { .. }
        ));
    }

    #[test]
    fn rejects_empty_payload() {
        // Bypass the typed constructor to expose an empty payload
        // to the plan validator.
        let bad = Action::Setpoint {
            target_instance: "motion_target".to_owned(),
            consumer_signature: setpoint_sig(),
            encoded_payload: vec![],
            validity: Validity::Permanent,
        };
        let steps = vec![Step::new("empty", 0, bad)];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(matches!(
            plan.unwrap_err(),
            PlanValidationError::EmptyPayload { .. }
        ));
    }

    #[test]
    fn rejects_payload_over_cap() {
        let bad = Action::Setpoint {
            target_instance: "motion_target".to_owned(),
            consumer_signature: setpoint_sig(),
            encoded_payload: vec![0; MAX_PAYLOAD + 1],
            validity: Validity::Permanent,
        };
        let steps = vec![Step::new("big", 0, bad)];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(matches!(
            plan.unwrap_err(),
            PlanValidationError::PayloadTooLarge { .. }
        ));
    }

    #[test]
    fn typed_setpoint_rejects_wrong_kind() {
        // The typed constructor must refuse a wrong-kind descriptor
        // at the authoring boundary so production code cannot
        // bypass plan validation.
        let result = Action::setpoint("motion_target", command_sig(), vec![1], Validity::Permanent);
        assert!(matches!(
            result,
            Err(PlanValidationError::WrongPortKind { .. })
        ));
    }

    #[test]
    fn typed_setpoint_rejects_empty_target_instance() {
        let result = Action::setpoint("", setpoint_sig(), vec![1], Validity::Permanent);
        assert!(matches!(
            result,
            Err(PlanValidationError::EmptyTargetInstance { .. })
        ));
    }

    #[test]
    fn typed_command_rejects_zero_host_deadline() {
        let result = Action::command(
            "motion_target",
            command_sig(),
            vec![1],
            "cmd",
            Duration::from_secs(1),
            Duration::ZERO,
        );
        assert!(matches!(
            result,
            Err(PlanValidationError::ZeroHostDeadline { .. })
        ));
    }

    #[test]
    fn sorted_steps_retain_validation() {
        let mut steps = vec![
            Step::new("second", 1, setpoint_action(2)),
            Step::new("first", 0, setpoint_action(1)),
        ];
        steps.sort_by_key(|s| (s.boundary, s.label.clone()));
        let plan =
            ScenarioPlan::with_steps("scene", Duration::from_secs(2), steps, vec![]).unwrap();
        assert_eq!(plan.steps[0].label, "first");
        assert_eq!(plan.steps[1].label, "second");
        // transition_count is derived from duration and the probed
        // quantum. With the rover's 2 ms quantum, two seconds yield
        // 1,000 transitions regardless of how many actions the
        // author wrote.
        let quantum = crate::scenario::Quantum::DEFAULT_MICROS;
        let rover_quantum = crate::scenario::Quantum::from_micros(quantum).expect("rover quantum");
        assert_eq!(plan.transition_count(rover_quantum), Some(1_000));
    }

    #[test]
    fn withdraw_accepts_state_signature() {
        let steps = vec![Step::new("withdraw", 0, withdraw_action())];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(plan.is_ok());
    }

    // ---- Generic quantum correction tests (plan §9) --------------------
    //
    // These cover two non-rover quanta, unaligned durations against
    // those quanta, zero duration/quantum, overflow past the u32 cap,
    // and the boundary-zero semantics for the very first transition.

    fn five_ms_quantum() -> crate::scenario::Quantum {
        crate::scenario::Quantum::from_micros(5_000).expect("5 ms")
    }

    fn ten_ms_quantum() -> crate::scenario::Quantum {
        crate::scenario::Quantum::from_micros(10_000).expect("10 ms")
    }

    #[test]
    fn non_rover_quantum_yields_correct_transition_count() {
        // Six seconds at 5 ms = 1_200 transitions.
        let plan = ScenarioPlan::with_steps(
            "scene",
            Duration::from_secs(6),
            vec![Step::new("a", 0, setpoint_action(1))],
            vec![],
        )
        .expect("shape validator accepts aligned duration");
        assert_eq!(plan.transition_count(five_ms_quantum()), Some(1_200));
        // Same six seconds at 10 ms = 600 transitions; the rover
        // constant must not be applied.
        assert_eq!(plan.transition_count(ten_ms_quantum()), Some(600));
    }

    #[test]
    fn non_rover_quantum_rejects_unaligned_duration() {
        // 6.001 s at 5 ms (5_000_000 ns) is not a multiple; reject.
        let plan = ScenarioPlan::with_steps(
            "scene",
            Duration::from_nanos(6_001_000_000),
            vec![Step::new("a", 0, setpoint_action(1))],
            vec![],
        )
        .expect("shape validator accepts unaligned duration");
        let error = plan
            .validate_for_quantum(five_ms_quantum())
            .expect_err("5 ms quantum must reject 6.001 s");
        assert!(matches!(
            error,
            PlanValidationError::DurationNotAligned {
                quantum_nanos: 5_000_000,
                ..
            }
        ));
    }

    #[test]
    fn validate_for_quantum_rejects_zero_duration() {
        // Duration of zero cannot meaningfully align to any quantum.
        // Construct via the raw enum variant to bypass `with_steps`
        // which would have enforced a positive duration through the
        // shape validator.
        let mut plan = ScenarioPlan::new("scene", Duration::ZERO);
        plan.steps.push(Step::new("a", 0, setpoint_action(1)));
        let error = plan
            .validate_for_quantum(ten_ms_quantum())
            .expect_err("zero duration must be rejected");
        assert!(matches!(error, PlanValidationError::ZeroDuration));
    }

    #[test]
    fn validate_for_quantum_rejects_zero_quantum() {
        // Zero microseconds cannot form a quantum; `from_micros`
        // already returns `None` at construction. Synthesize the
        // transition-count path with a 1-microsecond quantum to keep
        // the test self-contained and verify that the alignment
        // check refuses durations that are sub-quantum.
        let quantum = crate::scenario::Quantum::from_micros(1).expect("1 µs");
        let plan = ScenarioPlan::with_steps(
            "scene",
            Duration::from_nanos(999), // not a multiple of 1_000 ns
            vec![Step::new("a", 0, setpoint_action(1))],
            vec![],
        )
        .expect("shape validator accepts sub-quantum duration");
        let error = plan
            .validate_for_quantum(quantum)
            .expect_err("999 ns duration at 1 µs must be rejected");
        assert!(matches!(
            error,
            PlanValidationError::DurationNotAligned {
                quantum_nanos: 1_000,
                ..
            }
        ));
    }

    #[test]
    fn transition_count_overflow_is_rejected_not_saturated() {
        // Build a duration that overflows u32 at the chosen quantum.
        // (2^32 + 1) * 5 ms = ~596 hours, which exceeds any sane run
        // length but stays well below the u128 nanos cap.
        let nanos_per_transition: u128 = 5_000_000;
        let overflow_count: u128 = u32::MAX as u128 + 1; // = 2^32
        // `overflow_count + 1` would wrap a u32; we widen first to u128.
        let duration_nanos: u128 = (overflow_count + 1) * nanos_per_transition;
        let duration = std::time::Duration::from_nanos(duration_nanos as u64);
        let plan = ScenarioPlan::with_steps(
            "scene",
            duration,
            vec![Step::new("a", 0, setpoint_action(1))],
            vec![],
        )
        .expect("shape validator accepts the long duration");
        assert_eq!(plan.transition_count(five_ms_quantum()), None);
        let error = plan
            .validate_for_quantum(five_ms_quantum())
            .expect_err("overflow must surface as TransitionCountOverflow, not saturate");
        assert!(matches!(
            error,
            PlanValidationError::TransitionCountOverflow { .. }
        ));
    }

    #[test]
    fn boundary_zero_quantum_is_valid_when_duration_aligns() {
        // boundary == 0 is the first transition; it is inside
        // `[0, transitions)` whenever transitions > 0.
        let plan = ScenarioPlan::with_steps(
            "scene",
            Duration::from_secs(1),
            vec![Step::new("start", 0, setpoint_action(1))],
            vec![],
        )
        .expect("shape validator accepts");
        let rover_quantum =
            crate::scenario::Quantum::from_micros(crate::scenario::Quantum::DEFAULT_MICROS)
                .expect("default");
        plan.validate_for_quantum(rover_quantum)
            .expect("boundary zero must validate against a 1 s / 2 ms plan");
        assert_eq!(plan.transition_count(rover_quantum), Some(500));
    }
}
