//! Typed scenario plan: finite quantum conversion, deterministic
//! ordering, explicit validity, and bounded occurrences. P1 stub:
//! a scene path and duration. P2 fills the action schedule, capture
//! declarations, and validation that the schedule fits inside the
//! requested duration.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use phoxal_port::PortSignature;

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
    /// returned plan is normalized (deterministic ordering, unique
    /// labels, bounded command occurrences, quantum indices inside
    /// `[0, steps.len())`).
    pub fn with_steps(
        scene: impl Into<PathBuf>,
        duration: Duration,
        mut steps: Vec<Step>,
        captures: Vec<Capture>,
    ) -> Result<Self, PlanValidationError> {
        steps.sort_by_key(|step| (step.boundary, step.label.clone()));
        let plan = Self {
            scene: scene.into(),
            duration,
            steps,
            captures,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Returns the total transition count of the schedule. The plan is
    /// quantized to `steps.len()` transitions; every step's
    /// `boundary` must lie inside `[0, steps.len())`.
    pub fn transition_count(&self) -> u32 {
        self.steps.len() as u32
    }

    /// Validate the plan in place. Checks unique labels, command-label
    /// uniqueness across the schedule, quantum indices inside the
    /// transition count, finite duration, and that every encoded
    /// payload is bounded.
    pub fn validate(&self) -> Result<(), PlanValidationError> {
        if self.duration.is_zero() {
            return Err(PlanValidationError::ZeroDuration);
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
        consumer_signature: PortSignature,
        encoded_payload: Vec<u8>,
    },
    /// Withdraw the targeted producer's published state without
    /// supplying a replacement.
    Withdraw { producer_signature: PortSignature },
    /// Submit a request to a commands-style service and observe the
    /// correlation id through the controlled boundary. Advancement
    /// continues while the reply is pending.
    Command {
        service_signature: PortSignature,
        request_encoded: Vec<u8>,
        /// Stable label used to correlate command with reply.
        label: String,
    },
}

impl Action {
    fn validate(&self, step_label: &str) -> Result<(), PlanValidationError> {
        match self {
            Action::Setpoint {
                consumer_signature,
                encoded_payload,
            } => {
                if consumer_signature.kind != phoxal_port::PortKind::Setpoint {
                    return Err(PlanValidationError::WrongPortKind {
                        step_label: step_label.to_owned(),
                        expected: phoxal_port::PortKind::Setpoint,
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
            }
            Action::Withdraw { producer_signature } => {
                if producer_signature.kind != phoxal_port::PortKind::Setpoint
                    && producer_signature.kind != phoxal_port::PortKind::State
                {
                    return Err(PlanValidationError::WrongPortKind {
                        step_label: step_label.to_owned(),
                        expected: phoxal_port::PortKind::Setpoint,
                        actual: producer_signature.kind,
                    });
                }
            }
            Action::Command {
                service_signature,
                request_encoded,
                label,
            } => {
                if service_signature.kind != phoxal_port::PortKind::Commands {
                    return Err(PlanValidationError::WrongPortKind {
                        step_label: step_label.to_owned(),
                        expected: phoxal_port::PortKind::Commands,
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
        expected: phoxal_port::PortKind,
        actual: phoxal_port::PortKind,
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
            phoxal_port::PortKind::State,
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
            phoxal_port::PortKind::Sample,
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
            phoxal_port::PortKind::Event,
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
                expected: phoxal_port::PortKind::Stream,
                actual: phoxal_port::PortKind::Stream,
            });
        }
        Ok(Self::NativeBody { name, units, frame })
    }
}

fn require_kind(
    constructor: &'static str,
    expected: phoxal_port::PortKind,
    actual: phoxal_port::PortKind,
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
        expected: phoxal_port::PortKind,
        actual: phoxal_port::PortKind,
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
}

impl std::fmt::Display for PlanValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroDuration => write!(f, "scenario plan duration must be finite"),
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
            phoxal_port::PortKind::Setpoint,
            "SetpointRequest",
            "SetpointReply",
        )
    }
    fn command_sig() -> PortSignature {
        PortSignature::new(
            "motion/cmd",
            "phoxal.motion",
            "Do",
            phoxal_port::PortKind::Commands,
            "CommandRequest",
            "CommandReply",
        )
    }
    fn state_sig() -> PortSignature {
        PortSignature::new(
            "motion/state",
            "phoxal.motion",
            "State",
            phoxal_port::PortKind::State,
            "State",
            "State",
        )
    }

    #[test]
    fn rejects_zero_duration() {
        let plan = ScenarioPlan::with_steps(
            "scene",
            Duration::ZERO,
            vec![Step::new(
                "a",
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                },
            )],
            vec![],
        );
        assert_eq!(plan.unwrap_err(), PlanValidationError::ZeroDuration);
    }

    #[test]
    fn rejects_quantum_at_or_beyond_final_transition() {
        let steps = vec![
            Step::new(
                "a",
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                },
            ),
            Step::new(
                "b",
                5,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![2],
                },
            ),
        ];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(2), steps, vec![]);
        match plan.unwrap_err() {
            PlanValidationError::QuantumOutOfRange {
                label,
                quantum,
                transitions,
            } => {
                assert_eq!(label, "b");
                assert_eq!(quantum, 5);
                // Two steps => transition_count == 2; quantum 5 is out of range.
                assert_eq!(transitions, 2);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_duplicate_step_labels() {
        let steps = vec![
            Step::new(
                "a",
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                },
            ),
            Step::new(
                "a",
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![2],
                },
            ),
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
            Step::new(
                "step_one",
                0,
                Action::Command {
                    service_signature: command_sig(),
                    request_encoded: vec![1],
                    label: "cmd".to_owned(),
                },
            ),
            Step::new(
                "step_two",
                0,
                Action::Command {
                    service_signature: command_sig(),
                    request_encoded: vec![2],
                    label: "cmd".to_owned(),
                },
            ),
        ];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(matches!(
            plan.unwrap_err(),
            PlanValidationError::DuplicateCommandLabel { .. }
        ));
    }

    #[test]
    fn rejects_wrong_port_kind_for_setpoint() {
        let steps = vec![Step::new(
            "bad",
            0,
            Action::Setpoint {
                consumer_signature: command_sig(),
                encoded_payload: vec![1],
            },
        )];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(matches!(
            plan.unwrap_err(),
            PlanValidationError::WrongPortKind { .. }
        ));
    }

    #[test]
    fn rejects_empty_payload() {
        let steps = vec![Step::new(
            "empty",
            0,
            Action::Setpoint {
                consumer_signature: setpoint_sig(),
                encoded_payload: vec![],
            },
        )];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(matches!(
            plan.unwrap_err(),
            PlanValidationError::EmptyPayload { .. }
        ));
    }

    #[test]
    fn rejects_payload_over_cap() {
        let steps = vec![Step::new(
            "big",
            0,
            Action::Setpoint {
                consumer_signature: setpoint_sig(),
                encoded_payload: vec![0; MAX_PAYLOAD + 1],
            },
        )];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(matches!(
            plan.unwrap_err(),
            PlanValidationError::PayloadTooLarge { .. }
        ));
    }

    #[test]
    fn sorted_steps_retain_validation() {
        let mut steps = vec![
            Step::new(
                "second",
                1,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![2],
                },
            ),
            Step::new(
                "first",
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                },
            ),
        ];
        steps.sort_by_key(|s| (s.boundary, s.label.clone()));
        let plan =
            ScenarioPlan::with_steps("scene", Duration::from_secs(2), steps, vec![]).unwrap();
        assert_eq!(plan.steps[0].label, "first");
        assert_eq!(plan.steps[1].label, "second");
        // Two steps => transition_count == 2.
        assert_eq!(plan.transition_count(), 2);
    }

    #[test]
    fn withdraw_accepts_state_signature() {
        let steps = vec![Step::new(
            "withdraw",
            0,
            Action::Withdraw {
                producer_signature: state_sig(),
            },
        )];
        let plan = ScenarioPlan::with_steps("scene", Duration::from_secs(1), steps, vec![]);
        assert!(plan.is_ok());
    }
}
