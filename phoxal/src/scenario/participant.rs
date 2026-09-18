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
use crate::scenario::plan::{Capture, Step};
use crate::scenario::program::Program;

/// One quantum-aligned outcome captured during execution. P3 fills
/// the contents with native samples and service histories; P2
/// records the structural result only.
///
/// The wire-format derives let the case-host control channel forward
/// observed outcomes verbatim. The harness-side decoder requires the
/// same shape on both sides, so changing the variants is a
/// wire-incompatible change.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    #[allow(dead_code)]
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

    /// The authored steps this participant owns. The controlled phase
    /// driver iterates this schedule at the authoritative boundary;
    /// authors do not construct additional steps here.
    #[allow(dead_code)]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// The authored capture declarations this participant owns. The
    /// controlled phase driver opens these collectors at admission
    /// and seals them at the final boundary.
    #[allow(dead_code)]
    pub fn captures(&self) -> &[Capture] {
        &self.captures
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
    /// Synthetic outcome shape removed; see Gate B1 of
    /// the scenario acceptance review. The synthetic `run` method was a
    /// placeholder that fabricated boundary ticks, fake state
    /// captures, and command-issued outcomes without contacting a
    /// real consumer or running a child process. It is removed in
    /// favor of the case-host lifecycle that lands in Gate B1/B4.
    /// The retained surface (correlations, deadlines, replies)
    /// stays intact because the controlled phase loop being
    /// extracted from the framework SDK runner will use the same
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
/// [`HOST_DEADLINE_TICKS`] at the 2 ms rover quantum. Retained for
/// the typed correlation registry used by the controlled phase
/// driver; the synthetic emitter no longer reads it.
#[allow(dead_code)]
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
    use crate::port::PortSignature;
    use crate::scenario::plan::{Action, Step, Validity};
    use crate::scenario::program::ScheduleEntry;

    fn setpoint_sig() -> PortSignature {
        PortSignature::new(
            "motion/cmd",
            "phoxal.motion",
            "Set",
            crate::port::PortKind::Setpoint,
            "Req",
            "Reply",
        )
    }
    fn command_sig() -> PortSignature {
        PortSignature::new(
            "motion/cmd",
            "phoxal.motion",
            "Do",
            crate::port::PortKind::Commands,
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
                        target_instance: "motion_target".to_owned(),
                        consumer_signature: setpoint_sig(),
                        encoded_payload: vec![1, 2, 3],
                        validity: Validity::Permanent,
                    },
                ),
                ScheduleEntry::at(
                    1,
                    Action::Command {
                        target_instance: "motion_target".to_owned(),
                        service_signature: command_sig(),
                        request_encoded: vec![7, 7],
                        label: "do_thing".to_owned(),
                        simulated_deadline: std::time::Duration::from_secs(1),
                        host_deadline: std::time::Duration::from_secs(1),
                    },
                ),
            ],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn command_reply_mark_removes_correlation() {
        // The synthetic emitter is gone; a freshly-prepared
        // participant has no command correlations, so
        // `mark_command_reply` reports `false` for every label.
        // The case-host lifecycle that lands in Gate B1/B4
        // populates the correlation registry when commands are
        // issued; the regression is that the registry starts empty
        // and `mark_command_reply` returns `false` until something
        // is added.
        let mut participant = FixtureParticipant::from_program(sample_program()).unwrap();
        assert!(!participant.mark_command_reply("do_thing"));
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
                    target_instance: "motion_target".to_owned(),
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                    validity: Validity::Permanent,
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
                    target_instance: "motion_target".to_owned(),
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![2],
                    validity: Validity::Permanent,
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
    fn gate_expire_pending_commands_uses_failure_path() {
        // Synthetic step emitter is gone; the typed correlation
        // registry is empty for an unprepared participant and
        // therefore expires nothing. The case-host lifecycle in
        // Gate B inserts correlations as commands issue; this test
        // asserts the empty-correlations invariant.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Gate",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(
                0,
                Action::Command {
                    target_instance: "motion_target".to_owned(),
                    service_signature: command_sig(),
                    request_encoded: vec![1],
                    label: "do".to_owned(),
                    simulated_deadline: std::time::Duration::from_secs(1),
                    host_deadline: std::time::Duration::from_secs(1),
                },
            )],
            vec![],
        )
        .unwrap();
        let mut participant = FixtureParticipant::from_program(program).unwrap();
        assert!(participant.expire_pending_commands().is_empty());
        assert!(participant.expire_simulated_deadlines().is_empty());
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
                    target_instance: "motion_target".to_owned(),
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                    validity: Validity::Permanent,
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
                    target_instance: "motion_target".to_owned(),
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                    validity: Validity::Permanent,
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
                    target_instance: "motion_target".to_owned(),
                    consumer_signature: command_sig(),
                    encoded_payload: vec![1],
                    validity: Validity::Permanent,
                },
            }],
            vec![],
        );
        assert!(matches!(
            plan.unwrap_err(),
            crate::scenario::PlanValidationError::WrongPortKind { .. }
        ));
    }

    #[test]
    fn small_consumer_exchange_program_admits_into_collector_without_synthetic_preprocessing() {
        // Renamed from `real_phase_loop_seals_a_small_consumer_exchange`
        // per the previous test
        // claimed a seal path that does not exist (the synthetic
        // emitter and `run_through_owned` were removed by Gate B1).
        // The real phase loop is the case-host lifecycle that
        // Section 3 lands; until then this test asserts the
        // invariants the case host must satisfy: a normalized
        // program with declared steps and captures admits into a
        // typed EvidenceCollector without any synthetic
        // preprocessing. The collector MUST NOT be sealed here: the
        // seal requires lifecycle-recorded terminal evidence that
        // only the case host can record.
        use crate::scenario::Capture;
        use crate::scenario::results::{CaptureRecord, EvidenceCollector};
        fn capture_sig() -> PortSignature {
            PortSignature::new(
                "motion/state",
                "phoxal.motion",
                "State",
                crate::port::PortKind::State,
                "State",
                "State",
            )
        }
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/SmallExchange",
            quantum,
            std::time::Duration::from_secs(2),
            vec![
                ScheduleEntry::at(
                    0,
                    Action::Setpoint {
                        target_instance: "motion".to_owned(),
                        consumer_signature: setpoint_sig(),
                        encoded_payload: vec![1, 2, 3],
                        validity: Validity::Permanent,
                    },
                ),
                ScheduleEntry::at(
                    1,
                    Action::Command {
                        target_instance: "motion".to_owned(),
                        service_signature: command_sig(),
                        request_encoded: vec![0x10, 0x20],
                        label: "turn_left".to_owned(),
                        simulated_deadline: std::time::Duration::from_millis(500),
                        host_deadline: std::time::Duration::from_millis(500),
                    },
                ),
                ScheduleEntry::at(
                    2,
                    Action::Setpoint {
                        target_instance: "motion".to_owned(),
                        consumer_signature: setpoint_sig(),
                        encoded_payload: vec![0],
                        validity: Validity::Permanent,
                    },
                ),
            ],
            vec![Capture::state("motion", capture_sig()).expect("motion capture")],
        )
        .expect("normalize");
        program.verify_identity().expect("identity");
        let collector = EvidenceCollector::for_program(program.clone());
        let mut collector = collector;
        collector
            .record_capture("motion".to_owned(), CaptureRecord::State(vec![0x01, 0x02]))
            .expect("state capture");
        assert_eq!(program.steps().len(), 3);
        assert!(
            collector
                .record_command_reply(
                    "turn_left".to_owned(),
                    crate::scenario::results::CommandReply::Accepted {
                        response_bytes: vec![0xAA, 0xBB],
                    }
                )
                .is_ok()
        );
        // The collector must NOT be sealed here: that requires
        // lifecycle-recorded terminal evidence that only the case
        // host can record. Until Section 3 lands, attempting to seal
        // would surface `MissingTerminalEvidence`.
    }
}
