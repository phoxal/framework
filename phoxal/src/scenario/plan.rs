//! Typed scenario plan: finite quantum conversion, deterministic
//! ordering, explicit validity, and bounded occurrences. P1 stub:
//! a scene path and duration. P2 fills the action schedule, capture
//! declarations, and validation that the schedule fits inside the
//! requested duration.

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
    /// Construct a minimal plan with no actions or captures. P1 tests
    /// use this form so the trait stays compile-stable while P2
    /// lands the richer builders.
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

    /// Returns the total transition count of the schedule. The
    /// transition count is derived from the schedule's effective
    /// quantum and its declared duration: a six-second experiment at
    /// the rover's two millisecond quantum yields 3,000 transitions,
    /// independent of how many actions the author wrote.
    ///
    /// This replaces the previous action-count-based timing where
    /// `transition_count()` returned `steps.len()`; that conflated
    /// authoring density with experiment length and made a six-second
    /// experiment with one setpoint look like a single-transition
    /// program.
    ///
    /// Uses checked nanosecond arithmetic so sub-microsecond
    /// durations cannot be silently truncated to a whole-microsecond
    /// multiple. Unaligned durations fall back to zero transitions;
    /// `validate` rejects unaligned durations at construction, so
    /// this only fires for callers that bypass the validator.
    pub fn transition_count(&self) -> u32 {
        let quantum_nanos: u128 = 2_000_000; // 2 ms rover quantum
        let total_nanos = self.duration.as_nanos();
        if total_nanos == 0 || !total_nanos.is_multiple_of(quantum_nanos) {
            return 0;
        }
        let transitions = total_nanos / quantum_nanos;
        u32::try_from(transitions).unwrap_or(u32::MAX)
    }

    /// Validate the plan in place. Checks unique labels, command-label
    /// uniqueness across the schedule, quantum indices inside the
    /// transition count, finite duration, and that every encoded
    /// payload is bounded.
    pub fn validate(&self) -> Result<(), PlanValidationError> {
        if self.duration.is_zero() {
            return Err(PlanValidationError::ZeroDuration);
        }
        // Reject unaligned durations at construction. The transition
        // count below is computed from `duration / quantum_nanos` and
        // would otherwise fall back to zero (per the new
        // `transition_count` rule); an aligned duration is required
        // by Gate B4 of the scenario acceptance review.
        let quantum_nanos: u128 = 2_000_000;
        let total_nanos = self.duration.as_nanos();
        if !total_nanos.is_multiple_of(quantum_nanos) {
            return Err(PlanValidationError::DurationNotAligned {
                nanos: total_nanos,
                quantum_nanos,
            });
        }
        if self.steps.len() > u32::MAX as usize {
            return Err(PlanValidationError::TooManySteps(self.steps.len()));
        }
        let transition_count = self.transition_count();
        let mut seen_labels: BTreeMap<&str, &Step> = BTreeMap::new();
        let mut seen_commands: BTreeMap<&str, &Step> = BTreeMap::new();
        for step in &self.steps {
            if step.boundary >= transition_count {
                return Err(PlanValidationError::QuantumOutOfRange {
                    label: step.label.clone(),
                    quantum: step.boundary,
                    transitions: transition_count,
                });
            }
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

/// The action vocabulary enforced by P2.
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

/// Authoritative validity of a setpoint's resulting intent. P2 ships
/// only the persistent variant; finite-lifetime intent (timeout-bound
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
    /// The plan's duration is not an exact multiple of the 2 ms
    /// quantum. Authoring schedules that don't align to the quantum
    /// cannot be admitted: a 1 ns slippage could truncate to a
    /// whole-microsecond boundary that looks aligned. See Gate B4.
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
        // quantum. The validator must refuse it.
        let plan = ScenarioPlan::with_steps(
            "scene",
            Duration::from_nanos(2_001),
            vec![Step::new("a", 0, setpoint_action(1))],
            vec![],
        );
        assert!(matches!(
            plan.unwrap_err(),
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
        // valid range is `[0, transition_count)`.
        let steps = vec![
            Step::new("a", 0, setpoint_action(1)),
            Step::new("b", 1_000, setpoint_action(2)),
        ];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(2), steps, vec![]);
        match plan.unwrap_err() {
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
        // transition_count is derived from duration and the
        // 2 ms quantum, not from the action count. Two seconds at
        // 2 ms = 1,000 transitions regardless of how many actions
        // the author wrote.
        assert_eq!(plan.transition_count(), 1_000);
    }

    #[test]
    fn withdraw_accepts_state_signature() {
        let steps = vec![Step::new("withdraw", 0, withdraw_action())];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(plan.is_ok());
    }
}
