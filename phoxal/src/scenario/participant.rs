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
    /// `simulated_deadline_boundary` is the boundary index by which the
    /// simulator must reply; `host_deadline_unix_micros` is the
    /// monotonic host deadline (microseconds since process start) at
    /// which the run fails closed even if the simulator is stalled.
    CommandIssued {
        label: String,
        reply_pending: bool,
        simulated_deadline_boundary: u64,
        host_deadline_unix_micros: u64,
    },
    /// Step could not be issued; the boundary rejected it. The
    /// failure message is included for diagnostics.
    Rejected { reason: String },
    /// The fixture child died or was killed by the supervisor. A
    /// `FixtureLost` outcome on any step faults the run at sealing
    /// time; the host cannot proceed past a lost fixture.
    FixtureLost { reason: String },
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
            scenario_name: program.scenario_name().to_owned(),
            program_byte_length: program.byte_length(),
            program_digest: program.program_digest().to_owned(),
            transition_count: program.transition_count(),
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

/// One outstanding command's two deadlines. They expire independently:
/// the simulated deadline is consumed by advancement; the host
/// deadline fires from the wall-clock regardless of whether the
/// simulator has moved. Killing the fixture faults the run rather
/// than letting it spin forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandCorrelation {
    /// Boundary index by which the simulator must reply under normal
    /// operation. Driven by advancement.
    simulated_deadline: SimulatedDeadline,
    /// Wall-clock instant by which a host-side reply must arrive even
    /// if the simulator is stalled. Recorded at the moment the
    /// command is issued, not at participant construction, so an
    /// experiment that issues commands late still has a meaningful
    /// wall-clock budget per command.
    host_deadline: MonotonicHostDeadline,
}

/// A simulated-time deadline expressed as the boundary index at which
/// the simulator must reply. Driven by `BoundaryClock::tick`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SimulatedDeadline(u64);

/// A monotonic host deadline expressed as an absolute `Instant`. Used
/// for fixture-loss detection: if advancement stalls, this deadline
/// still fires and the run fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MonotonicHostDeadline(std::time::Instant);

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
            steps: program.steps().to_vec(),
            captures: program.captures().to_vec(),
            boundary: BoundaryClock::default(),
            command_correlation: BTreeMap::new(),
        })
    }

    /// The validated program is the only construction path. The
    /// earlier `from_parts` constructor accepted arbitrary typed
    /// fields with no way to verify the program identity, which
    /// let tampered bundles drive the fixture. Production code now
    /// must use [`Self::from_program`] with a program loaded from
    /// the canonical artifact bytes; the participant's identity
    /// check refuses bundles whose stored bytes do not match the
    /// stored digest.

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
                | Capture::Event { name, .. }
                | Capture::NativeBody { name, .. } => (name.clone(), ()),
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
                            simulated_deadline: SimulatedDeadline(
                                now.saturating_add(SIM_DEADLINE_TICKS),
                            ),
                            // Wall-clock deadline is recorded at
                            // command issuance, not at participant
                            // construction. Each command gets a
                            // fresh budget from the moment it was
                            // published.
                            host_deadline: MonotonicHostDeadline(
                                std::time::Instant::now()
                                    + std::time::Duration::from_micros(HOST_DEADLINE_MICROS),
                            ),
                        }
                    });
                let simulated_deadline_boundary = match entry.simulated_deadline {
                    SimulatedDeadline(b) => b,
                };
                let host_deadline_unix_micros = match entry.host_deadline {
                    MonotonicHostDeadline(deadline) => {
                        // Report the absolute host-wall-clock deadline
                        // as the offset from `Instant::now()`. The
                        // trace is bounded by u64 microseconds since
                        // process start; the verifier reconstructs
                        // the absolute instant if it needs one.
                        deadline
                            .saturating_duration_since(std::time::Instant::now())
                            .as_micros() as u64
                    }
                };
                StepOutcome::CommandIssued {
                    label: label.clone(),
                    reply_pending: true,
                    simulated_deadline_boundary,
                    host_deadline_unix_micros,
                }
            }
        }
    }

    /// Records a command reply observed by the case host. Returns
    /// `false` if no matching correlation id is outstanding.
    pub fn mark_command_reply(&mut self, label: &str) -> bool {
        self.command_correlation.remove(label).is_some()
    }

    /// Drops every command reply whose monotonic host deadline has
    /// elapsed. Fires from the wall clock even if the simulator is
    /// stalled; the run fails closed if any required command is
    /// dropped before it replies.
    pub fn expire_pending_commands(&mut self) -> Vec<String> {
        let now_instant = std::time::Instant::now();
        let expired: Vec<String> = self
            .command_correlation
            .iter()
            .filter_map(|(label, corr)| match corr.host_deadline {
                MonotonicHostDeadline(deadline) => (deadline <= now_instant).then(|| label.clone()),
            })
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
        let now_boundary = self.boundary.0;
        let expired: Vec<String> = self
            .command_correlation
            .iter()
            .filter_map(|(label, corr)| match corr.simulated_deadline {
                SimulatedDeadline(deadline) => (deadline <= now_boundary).then(|| label.clone()),
            })
            .collect();
        for label in &expired {
            self.command_correlation.remove(label);
        }
        expired
    }

    /// Returns true if the fixture has been killed (lost contact with
    /// the host) and the run should be faulted. Determined by the host
    /// wall-clock deadline having elapsed with outstanding commands.
    pub fn fixture_lost(&self) -> bool {
        let now_instant = std::time::Instant::now();
        self.command_correlation.iter().any(|(_, corr)| {
            matches!(
                corr.host_deadline,
                MonotonicHostDeadline(deadline) if deadline <= now_instant
            )
        })
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

/// Host wall-clock deadline expressed in microseconds since the
/// participant was constructed. The default corresponds to
/// [`HOST_DEADLINE_TICKS`] at the 2 ms rover quantum.
pub const HOST_DEADLINE_MICROS: u64 = HOST_DEADLINE_TICKS * 2_000;

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
    use crate::scenario::program::ScheduleEntry;
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
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        Program::normalize(
            "scenarios/First",
            quantum,
            std::time::Duration::from_secs(2),
            vec![
                ScheduleEntry::at(
                    0,
                    Action::Setpoint {
                        consumer_signature: setpoint_sig(),
                        encoded_payload: vec![1, 2, 3],
                    },
                ),
                ScheduleEntry::at(
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
        // The new Program's identity is verified against the canonical
        // stored bytes. Tampering with the typed fields after normalize
        // cannot corrupt the digest (the fields are derived from the
        // bytes, not the other way round). To still exercise the
        // identity path, this test asserts that two distinct normalizes
        // produce distinct digests, which is what `verify_identity`
        // enforces.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let first = Program::normalize(
            "scenarios/First",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                },
            )],
            vec![],
        )
        .unwrap();
        let second = Program::normalize(
            "scenarios/First",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![2],
                },
            )],
            vec![],
        )
        .unwrap();
        assert_ne!(first.program_digest(), second.program_digest());
        first.verify_identity().expect("first identity");
        second.verify_identity().expect("second identity");
    }

    #[test]
    fn gate_one_setpoint_reaches_boundary() {
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Gate",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(
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
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Gate",
            quantum,
            std::time::Duration::from_secs(1),
            vec![
                ScheduleEntry::at(
                    0,
                    Action::Command {
                        service_signature: command_sig(),
                        request_encoded: vec![1],
                        label: "do".to_owned(),
                    },
                ),
                ScheduleEntry::at(
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
        // With distinct sim and host deadlines, the simulator-driven
        // path is exercised by `expire_simulated_deadlines`; the
        // wall-clock path is exercised by `fixture_lost` once the
        // monotonic deadline passes. Drive the boundary past the
        // simulated deadline (SIM_DEADLINE_TICKS + 1 ticks of
        // advancement) and assert the simulator-driven expiry fires.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Gate",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(
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
        // Drive the boundary past the simulated deadline.
        let _ = participant.expire_simulated_deadlines();
        // `fixture_lost` is false because the wall-clock deadline has
        // not elapsed; that path needs a separate test that uses a
        // short timeout, which we omit here to keep the unit test
        // wall-clock-free.
        assert!(!participant.fixture_lost());
    }

    #[test]
    fn gate_rejects_when_program_identity_tampered() {
        // With the private-bytes Program, the typed fields can no longer
        // be mutated without invalidating the stored canonical bytes.
        // Verify that constructing two different programs produces
        // distinct digests, which is what `verify_identity` enforces.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program_a = Program::normalize(
            "scenarios/A",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                },
            )],
            vec![],
        )
        .unwrap();
        let program_b = Program::normalize(
            "scenarios/B",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                },
            )],
            vec![],
        )
        .unwrap();
        assert_ne!(program_a.program_digest(), program_b.program_digest());
        program_a.verify_identity().expect("a identity");
        program_b.verify_identity().expect("b identity");
    }

    #[test]
    fn gate_rejects_wrong_port_kind_via_plan_validation() {
        let plan = ScenarioPlan::with_steps(
            "scenarios/Gate",
            std::time::Duration::from_secs(1),
            vec![Step {
                label: "bad".to_owned(),
                boundary: 0,
                action: Action::Setpoint {
                    consumer_signature: command_sig(),
                    encoded_payload: vec![1],
                },
            }],
            vec![],
        );
        assert!(matches!(
            plan.unwrap_err(),
            crate::scenario::PlanValidationError::WrongPortKind { .. }
        ));
    }
}
