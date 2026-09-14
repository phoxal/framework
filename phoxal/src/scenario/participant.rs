//! Prepared fixture participant. P2.
//!
//! A `FixtureParticipant` consumes one validated [`Program`] and drives
//! its action schedule through the controlled-phase driver. The
//! participant shares admission / initialisation / boundary
//! validation / reset / stop semantics with ordinary `RegisteredRuntime`
//! participants; the type only narrows the inputs to a single
//! immutable schedule the prepared bundle provides.
//!
//! Author callbacks (`Default`, `plan`, `verify`) never run inside
//! the scheduling loop — they are confined to the case host. The
//! participant is purely a typed transport surface over the prepared
//! `Program`'s encoded payloads.

use std::collections::BTreeMap;

#[cfg(test)]
use crate::scenario::plan::ScenarioPlan;
use crate::scenario::plan::{Action, Capture, MAX_PAYLOAD, Step};
use crate::scenario::program::Program;

/// One quantum-aligned outcome captured during execution. P3 fills
/// the contents with native samples and service histories; P2
/// records the structural result only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// Setpoint payload was delivered to the documented consumer
    /// boundary; `production` is the controlling boundary tick
    /// at which the producer recorded the eligibility, `eligibility`
    /// the tick at which the consumer accepted it.
    SetpointDelivered { production: u64, eligibility: u64 },
    /// Withdraw request was accepted by the controlled boundary.
    WithdrawAccepted,
    /// Command request was published and the correlation id was
    /// returned by the transport. The reply may still be pending.
    CommandIssued {
        label: String,
        reply_pending: bool,
        simulated_deadline: u64,
        host_deadline: u64,
    },
    /// Step could not be issued; the boundary rejected it. The
    /// failure message is included for diagnostics.
    Rejected { reason: String },
}

/// One full execution trace. P3 extends this with capture samples.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixtureTrace {
    pub step_outcomes: Vec<(String, StepOutcome)>,
    pub captured: Vec<(String, Vec<u8>)>,
}

impl FixtureTrace {
    pub fn passed(&self) -> bool {
        self.step_outcomes
            .iter()
            .all(|(_, outcome)| !matches!(outcome, StepOutcome::Rejected { .. }))
    }
}

/// Validated metadata the supervisor bundles with the fixture child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureMetadata {
    pub scenario_name: String,
    pub program_byte_length: u32,
    pub program_digest: String,
    pub transition_count: u32,
}

impl FixtureMetadata {
    pub fn from_program(program: &Program) -> Self {
        Self {
            scenario_name: program.scenario_name.clone(),
            program_byte_length: program.byte_length,
            program_digest: program.program_digest.clone(),
            transition_count: program.steps.len() as u32,
        }
    }
}

/// Drives the prepared fixture through its quantized schedule. The
/// driver holds no author callbacks; every action comes from the
/// bundled program.
#[derive(Debug)]
pub struct FixtureParticipant {
    metadata: FixtureMetadata,
    steps: Vec<Step>,
    captures: Vec<Capture>,
    boundary: BoundaryClock,
    command_correlation: BTreeMap<String, CommandCorrelation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandCorrelation {
    simulated_deadline: u64,
    host_deadline: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct BoundaryClock(u64);

impl BoundaryClock {
    fn tick(&mut self) -> u64 {
        let now = self.0;
        self.0 = self.0.saturating_add(1);
        now
    }
}

impl FixtureParticipant {
    /// Construct a participant from a verified program. Refuses
    /// programs whose identity check fails so a tampered bundle
    /// cannot drive the fixture.
    pub fn from_program(program: Program) -> Result<Self, FixtureError> {
        program.verify_identity()?;
        Ok(Self {
            metadata: FixtureMetadata::from_program(&program),
            steps: program.steps,
            captures: program.captures,
            boundary: BoundaryClock::default(),
            command_correlation: BTreeMap::new(),
        })
    }

    /// Construct a participant from metadata + the original action
    /// schedule. Used when the bundle re-assembles the program from
    /// the on-disk JSON envelope.
    pub fn from_parts(
        metadata: FixtureMetadata,
        steps: Vec<Step>,
        captures: Vec<Capture>,
    ) -> Result<Self, FixtureError> {
        if steps.len() as u32 != metadata.transition_count {
            return Err(FixtureError::TransitionCountMismatch {
                declared: metadata.transition_count,
                actual: steps.len() as u32,
            });
        }
        Ok(Self {
            metadata,
            steps,
            captures,
            boundary: BoundaryClock::default(),
            command_correlation: BTreeMap::new(),
        })
    }

    pub fn metadata(&self) -> &FixtureMetadata {
        &self.metadata
    }

    /// Runs the schedule to completion. Each `StepOutcome` is
    /// produced in program order; the trace captures the result of
    /// each boundary interaction. `CommandIssued` outcomes keep
    /// their reply pending — the caller observes the produced
    /// trace to confirm transport acknowledgement, then calls
    /// [`Self::mark_command_reply`] when the reply lands (or
    /// [`Self::expire_pending_commands`] when the host deadline
    /// passes).
    pub fn run(&mut self) -> FixtureTrace {
        let mut trace = FixtureTrace::default();
        for step in self.steps.clone() {
            trace
                .step_outcomes
                .push((step.label.clone(), self.execute_step(&step)));
        }
        // Capture declarations produce empty native samples in P2;
        // P3 fills them with the recorded observation histories.
        for capture in &self.captures {
            let (name, _) = match capture {
                Capture::State { name, .. }
                | Capture::Sample { name, .. }
                | Capture::Event { name, .. } => (name.clone(), ()),
            };
            trace.captured.push((name, Vec::new()));
        }
        trace
    }

    fn execute_step(&mut self, step: &Step) -> StepOutcome {
        let production = self.boundary.tick();
        match &step.action {
            Action::Setpoint {
                encoded_payload, ..
            } => {
                if encoded_payload.len() > MAX_PAYLOAD {
                    return StepOutcome::Rejected {
                        reason: format!(
                            "setpoint payload {} bytes exceeds {} cap",
                            encoded_payload.len(),
                            MAX_PAYLOAD
                        ),
                    };
                }
                let eligibility = self.boundary.tick();
                StepOutcome::SetpointDelivered {
                    production,
                    eligibility,
                }
            }
            Action::Withdraw { .. } => {
                let _ = self.boundary.tick();
                StepOutcome::WithdrawAccepted
            }
            Action::Command {
                request_encoded,
                label,
                ..
            } => {
                if request_encoded.len() > MAX_PAYLOAD {
                    return StepOutcome::Rejected {
                        reason: format!(
                            "command `{label}` payload {} bytes exceeds {} cap",
                            request_encoded.len(),
                            MAX_PAYLOAD
                        ),
                    };
                }
                let entry = self
                    .command_correlation
                    .entry(label.clone())
                    .or_insert_with(|| {
                        let now = self.boundary.0;
                        CommandCorrelation {
                            simulated_deadline: now.saturating_add(SIM_DEADLINE_TICKS),
                            host_deadline: now.saturating_add(HOST_DEADLINE_TICKS),
                        }
                    });
                let simulated_deadline = entry.simulated_deadline;
                let host_deadline = entry.host_deadline;
                StepOutcome::CommandIssued {
                    label: label.clone(),
                    reply_pending: true,
                    simulated_deadline,
                    host_deadline,
                }
            }
        }
    }

    /// Records a command reply observed by the case host. Returns
    /// `false` if no matching correlation id is outstanding.
    pub fn mark_command_reply(&mut self, label: &str) -> bool {
        self.command_correlation.remove(label).is_some()
    }

    /// Drops every command reply whose host deadline has elapsed.
    /// Used when the simulator advances past the host wall-clock
    /// deadline before the reply arrives.
    pub fn expire_pending_commands(&mut self) -> Vec<String> {
        let now = self.boundary.0;
        let expired: Vec<String> = self
            .command_correlation
            .iter()
            .filter(|(_, corr)| corr.host_deadline <= now)
            .map(|(label, _)| label.clone())
            .collect();
        for label in &expired {
            self.command_correlation.remove(label);
        }
        expired
    }

    /// Drops every command reply whose simulated deadline has
    /// elapsed. Used when the simulator advances past the simulated
    /// deadline while the host wall clock still permits a real
    /// reply; the simulated experiment continues without it.
    pub fn expire_simulated_deadlines(&mut self) -> Vec<String> {
        let now = self.boundary.0;
        let expired: Vec<String> = self
            .command_correlation
            .iter()
            .filter(|(_, corr)| corr.simulated_deadline <= now)
            .map(|(label, _)| label.clone())
            .collect();
        for label in &expired {
            self.command_correlation.remove(label);
        }
        expired
    }

    /// Resets the participant to a clean post-construction state. The
    /// metadata is preserved; the boundary clock and pending
    /// commands are reset so the same prepared program can be
    /// re-driven under a new scenario iteration.
    pub fn reset(&mut self) {
        self.boundary = BoundaryClock::default();
        self.command_correlation.clear();
    }
}

/// Number of boundary advances after which the simulated experiment's
/// per-command deadline fires. Tuned conservatively; real hardware
/// replies are bounded by [`HOST_DEADLINE_TICKS`].
pub const SIM_DEADLINE_TICKS: u64 = 32;
/// Number of boundary advances after which the host wall-clock
/// deadline fires. The supervisor fails the fixture child if it
/// observes a host-deadline expiry before the reply lands.
pub const HOST_DEADLINE_TICKS: u64 = 256;

/// Errors returned by the prepared fixture participant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureError {
    IdentityMismatch(String),
    TransitionCountMismatch { declared: u32, actual: u32 },
}

impl std::fmt::Display for FixtureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdentityMismatch(message) => write!(f, "{message}"),
            Self::TransitionCountMismatch { declared, actual } => write!(
                f,
                "transition count mismatch: declared {declared}, actual {actual}"
            ),
        }
    }
}

impl std::error::Error for FixtureError {}

impl From<crate::scenario::program::ProgramError> for FixtureError {
    fn from(error: crate::scenario::program::ProgramError) -> Self {
        Self::IdentityMismatch(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::plan::{Action, Step};
    use phoxal_port::PortSignature;

    fn setpoint_sig() -> PortSignature {
        PortSignature::new(
            "motion/cmd",
            "phoxal.motion",
            "Set",
            phoxal_port::PortKind::Setpoint,
            "Req",
            "Reply",
        )
    }
    fn command_sig() -> PortSignature {
        PortSignature::new(
            "motion/cmd",
            "phoxal.motion",
            "Do",
            phoxal_port::PortKind::Commands,
            "Req",
            "Reply",
        )
    }

    fn sample_program() -> Program {
        Program::normalize(
            "scenarios/First",
            std::time::Duration::from_secs(2),
            vec![
                Step::new(
                    "set",
                    0,
                    Action::Setpoint {
                        consumer_signature: setpoint_sig(),
                        encoded_payload: vec![1, 2, 3],
                    },
                ),
                Step::new(
                    "do",
                    1,
                    Action::Command {
                        service_signature: command_sig(),
                        request_encoded: vec![7, 7],
                        label: "do_thing".to_owned(),
                    },
                ),
            ],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn run_records_each_step_outcome() {
        let mut participant = FixtureParticipant::from_program(sample_program()).unwrap();
        let trace = participant.run();
        assert_eq!(trace.step_outcomes.len(), 2);
        assert!(trace.passed());
        assert!(matches!(
            trace.step_outcomes[0].1,
            StepOutcome::SetpointDelivered { .. }
        ));
        assert!(matches!(
            trace.step_outcomes[1].1,
            StepOutcome::CommandIssued {
                reply_pending: true,
                ..
            }
        ));
    }

    #[test]
    fn command_reply_mark_removes_correlation() {
        let mut participant = FixtureParticipant::from_program(sample_program()).unwrap();
        let _ = participant.run();
        assert!(participant.mark_command_reply("do_thing"));
        assert!(!participant.mark_command_reply("do_thing"));
    }

    #[test]
    fn reset_restarts_boundary_clock() {
        let mut participant = FixtureParticipant::from_program(sample_program()).unwrap();
        let _ = participant.run();
        participant.reset();
        // After reset the next setpoint production is 0 again.
        let trace = participant.run();
        assert!(matches!(
            trace.step_outcomes[0].1,
            StepOutcome::SetpointDelivered { production: 0, .. }
        ));
    }

    #[test]
    fn refuses_identity_mismatch() {
        let mut program = sample_program();
        program.scenario_name = "scenarios/Other".to_owned();
        let result = FixtureParticipant::from_program(program);
        assert!(matches!(
            result.unwrap_err(),
            FixtureError::IdentityMismatch(_)
        ));
    }

    #[test]
    fn gate_one_setpoint_reaches_boundary() {
        let program = Program::normalize(
            "scenarios/Gate",
            std::time::Duration::from_secs(1),
            vec![Step::new(
                "set",
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                },
            )],
            vec![],
        )
        .unwrap();
        let mut participant = FixtureParticipant::from_program(program).unwrap();
        let trace = participant.run();
        assert_eq!(trace.step_outcomes.len(), 1);
        match &trace.step_outcomes[0].1 {
            StepOutcome::SetpointDelivered {
                production,
                eligibility,
            } => {
                assert_eq!(*production, 0);
                assert_eq!(*eligibility, 1);
            }
            other => panic!("expected setpoint delivery, got {other:?}"),
        }
        assert!(trace.passed());
    }

    #[test]
    fn gate_one_command_does_not_pause_advancement() {
        let program = Program::normalize(
            "scenarios/Gate",
            std::time::Duration::from_secs(1),
            vec![
                Step::new(
                    "do",
                    0,
                    Action::Command {
                        service_signature: command_sig(),
                        request_encoded: vec![1],
                        label: "do".to_owned(),
                    },
                ),
                Step::new(
                    "after",
                    1,
                    Action::Setpoint {
                        consumer_signature: setpoint_sig(),
                        encoded_payload: vec![2],
                    },
                ),
            ],
            vec![],
        )
        .unwrap();
        let mut participant = FixtureParticipant::from_program(program).unwrap();
        let trace = participant.run();
        // The command is left pending, yet the next step still ran:
        // advancement is not paused by the unreplied command.
        match &trace.step_outcomes[0].1 {
            StepOutcome::CommandIssued { reply_pending, .. } => assert!(reply_pending),
            other => panic!("expected command issued, got {other:?}"),
        }
        assert!(matches!(
            trace.step_outcomes[1].1,
            StepOutcome::SetpointDelivered { .. }
        ));
        assert!(trace.passed());
    }

    #[test]
    fn gate_expire_pending_commands_uses_failure_path() {
        let program = Program::normalize(
            "scenarios/Gate",
            std::time::Duration::from_secs(1),
            vec![Step::new(
                "do",
                0,
                Action::Command {
                    service_signature: command_sig(),
                    request_encoded: vec![1],
                    label: "do".to_owned(),
                },
            )],
            vec![],
        )
        .unwrap();
        let mut participant = FixtureParticipant::from_program(program).unwrap();
        let _ = participant.run();
        // Drive the boundary past the host deadline.
        for _ in 0..(HOST_DEADLINE_TICKS + 1) {
            let _ = participant.run();
        }
        let expired = participant.expire_pending_commands();
        assert_eq!(expired, vec!["do".to_owned()]);
    }

    #[test]
    fn gate_rejects_when_program_identity_tampered() {
        let mut program = sample_program();
        // Bypass `from_program` so we can construct a participant from
        // an identity that has been mutated after normalize.
        program.verify_identity().unwrap();
        program.scenario_name = "scenarios/Other".to_owned();
        let result = FixtureParticipant::from_program(program);
        assert!(matches!(
            result.unwrap_err(),
            FixtureError::IdentityMismatch(_)
        ));
    }

    #[test]
    fn gate_rejects_wrong_port_kind_via_plan_validation() {
        let plan = ScenarioPlan::with_steps(
            "scenarios/Gate",
            std::time::Duration::from_secs(1),
            vec![Step::new(
                "bad",
                0,
                Action::Setpoint {
                    consumer_signature: command_sig(),
                    encoded_payload: vec![1],
                },
            )],
            vec![],
        );
        assert!(matches!(
            plan.unwrap_err(),
            crate::scenario::PlanValidationError::WrongPortKind { .. }
        ));
    }
}
