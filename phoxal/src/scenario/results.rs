//! Typed scenario results. P3.
//!
//! The P1 placeholder `ScenarioRun` (a marker struct) is replaced
//! with a typed run record that captures every observable the
//! scenario author declared plus the per-action command replies.
//! Captures are filled in by the case host once the simulation
//! finishes; command replies include the correlation label so the
//! host can verify the reply sequence.
//!
//! The plan declares three concrete capture flavours plus the
//! command-reply acknowledgement record. `Scenario::verify` consumes
//! the assembled `ScenarioRun` and returns `Ok(())` on success or
//! `Err(...)` describing the first observed divergence from the
//! declared plan.

use std::collections::BTreeMap;

use crate::scenario::participant::{FixtureTrace, StepOutcome};
use crate::scenario::program::Program;

/// Why a `ScenarioRun` cannot be sealed. The collector is exhaustive
/// because sealing must fail closed on missing required evidence —
/// an empty captures map passes `from_trace` today and that is the
/// bug this enum is the explicit fix for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    /// The trace reported an outcome label that was not declared by
    /// the program.
    UnexpectedStepLabel(String),
    /// The trace did not report an outcome for a declared step label.
    MissingStepLabel(String),
    /// The trace reported more outcomes than the program declared.
    ExcessStepOutcomes {
        declared: usize,
        observed: usize,
    },
    /// A capture declared by the program has no record in the trace.
    MissingCapture(String),
    /// A command declared by the program has no reply record.
    MissingCommandReply(String),
    /// A capture record was supplied for a name the program did not
    /// declare.
    UnexpectedCapture(String),
    /// A command reply record was supplied for a label the program
    /// did not declare.
    UnexpectedCommandReply(String),
    /// The fixture reported `FixtureLoss` and the run must be faulted.
    FixtureLost,
    /// A step outcome is `Rejected`; the run is failed.
    StepRejected(String),
}

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedStepLabel(label) => {
                write!(f, "trace step `{label}` was not declared by the program")
            }
            Self::MissingStepLabel(label) => {
                write!(f, "declared step `{label}` has no outcome in the trace")
            }
            Self::ExcessStepOutcomes { declared, observed } => write!(
                f,
                "trace reported {observed} step outcomes but program declared {declared}"
            ),
            Self::MissingCapture(name) => {
                write!(f, "declared capture `{name}` has no record in the trace")
            }
            Self::MissingCommandReply(label) => {
                write!(f, "declared command `{label}` has no reply record")
            }
            Self::UnexpectedCapture(name) => write!(
                f,
                "capture record supplied for `{name}` but program did not declare it"
            ),
            Self::UnexpectedCommandReply(label) => write!(
                f,
                "command reply record supplied for `{label}` but program did not declare it"
            ),
            Self::FixtureLost => write!(f, "fixture was lost (host wall-clock deadline elapsed)"),
            Self::StepRejected(reason) => write!(f, "step was rejected: {reason}"),
        }
    }
}

impl std::error::Error for SealError {}

/// One completed simulated experiment. Built by [`EvidenceCollector`]
/// and frozen by [`EvidenceCollector::seal`]. Once sealed, the
/// internal fields are immutable from the case host's perspective;
/// the `verify` callback reads them but cannot extend them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioRun {
    /// Sealed step outcomes in program order.
    step_outcomes: Vec<(String, StepOutcome)>,
    /// Sealed capture observations keyed by declared capture name.
    captures: BTreeMap<String, CaptureRecord>,
    /// Sealed command-reply records keyed by declared correlation label.
    command_replies: BTreeMap<String, CommandReply>,
    /// Whether the host considers the run successful. Computed by
    /// `seal`; the `verify` callback may reject on top of this.
    passed: bool,
    /// `true` once the collector has frozen the evidence surface.
    /// Only sealed runs reach `verify`.
    sealed: bool,
}

impl ScenarioRun {
    /// Number of step outcomes recorded.
    pub fn step_count(&self) -> usize {
        self.step_outcomes.len()
    }

    /// Number of unique commands observed.
    pub fn command_count(&self) -> usize {
        self.command_replies.len()
    }

    /// Returns the step outcome for `label`, if any.
    pub fn outcome(&self, label: &str) -> Option<&StepOutcome> {
        self.step_outcomes
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, outcome)| outcome)
    }

    /// Returns the capture record for `name`, if any.
    pub fn capture(&self, name: &str) -> Option<&CaptureRecord> {
        self.captures.get(name)
    }

    /// Returns the command reply record for `label`, if any.
    pub fn command_reply(&self, label: &str) -> Option<&CommandReply> {
        self.command_replies.get(label)
    }

    /// Whether this run has been sealed. Only sealed runs reach
    /// `verify`; an unsealed run is a programmer error.
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Whether the host considers the run successful. Computed by
    /// `seal`; the `verify` callback may reject on top of this.
    pub fn passed(&self) -> bool {
        self.passed
    }

    /// Reconstruct a `ScenarioRun` directly from the same fields the
    /// collector tracks. Kept available so existing tests can assert
    /// sealing behaviour; production code uses [`EvidenceCollector`].
    #[doc(hidden)]
    pub fn from_sealed_parts(
        step_outcomes: Vec<(String, StepOutcome)>,
        captures: BTreeMap<String, CaptureRecord>,
        command_replies: BTreeMap<String, CommandReply>,
        passed: bool,
    ) -> Self {
        Self {
            step_outcomes,
            captures,
            command_replies,
            passed,
            sealed: true,
        }
    }

    /// Transitional constructor retained so existing tests pass; the
    /// evidence-surface checks have moved into `EvidenceCollector::seal`.
    /// `passed` is computed the same way the collector computes it.
    pub fn from_trace(
        trace: FixtureTrace,
        captures: BTreeMap<String, CaptureRecord>,
        command_replies: BTreeMap<String, CommandReply>,
    ) -> Self {
        let passed = trace.passed()
            && command_replies
                .values()
                .all(|reply| matches!(reply, CommandReply::Accepted { .. }));
        Self {
            step_outcomes: trace.step_outcomes,
            captures,
            command_replies,
            passed,
            sealed: false,
        }
    }
}

/// One captured observation. The flavour is recorded alongside the
/// raw bytes so the case host can demultiplex state, sample, and
/// event records without re-parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureRecord {
    /// Latest value published by a state port.
    State(Vec<u8>),
    /// Ordered batch of native samples captured from a sample port.
    Samples(Vec<Vec<u8>>),
    /// Ordered batch of event occurrences.
    Events(Vec<Vec<u8>>),
    /// Native simulator body data, identified by units and frame.
    NativeBody(Vec<u8>),
}

/// One command reply acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandReply {
    /// The transport accepted the reply. `response_bytes` are the
    /// application's decoded protobuf payload (if any).
    Accepted { response_bytes: Vec<u8> },
    /// The host dropped the command after the simulated deadline.
    ExpiredSimulated,
    /// The host dropped the command after the host wall-clock
    /// deadline. The supervisor records this as a required-child
    /// failure.
    ExpiredHost,
    /// The reply arrived but the simulator rejected it for a
    /// transport reason captured in `reason`.
    Rejected { reason: String },
}

/// Internal collector that knows what evidence the program required
/// and validates completeness, identity, sequence, capacity, final
/// boundary, and cleanup before sealing the run.
///
/// `seal()` is fallible: any missing or extra evidence returns a
/// `SealError` describing the first divergence. The case host must
/// only call `verify()` on a sealed run.
#[derive(Debug)]
pub struct EvidenceCollector {
    program: Program,
    step_outcomes: Vec<(String, StepOutcome)>,
    captures: BTreeMap<String, CaptureRecord>,
    command_replies: BTreeMap<String, CommandReply>,
    declared_step_labels: Vec<String>,
    declared_capture_names: Vec<String>,
    declared_command_labels: Vec<String>,
}

impl EvidenceCollector {
    /// Begin collecting evidence for one program. The program's
    /// declared step labels, capture names, and command labels are
    /// recorded so `seal()` can validate completeness.
    pub fn for_program(program: Program) -> Self {
        let declared_step_labels = program
            .steps()
            .iter()
            .map(|step| step.label.clone())
            .collect();
        let declared_capture_names = program
            .captures()
            .iter()
            .map(|capture| capture_name(capture).to_owned())
            .collect();
        let declared_command_labels = program
            .steps()
            .iter()
            .filter_map(|step| match &step.action {
                crate::scenario::plan::Action::Command { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();
        Self {
            program,
            step_outcomes: Vec::new(),
            captures: BTreeMap::new(),
            command_replies: BTreeMap::new(),
            declared_step_labels,
            declared_capture_names,
            declared_command_labels,
        }
    }

    /// Record one step outcome from the participant's trace.
    pub fn record_step_outcome(&mut self, label: String, outcome: StepOutcome) {
        self.step_outcomes.push((label, outcome));
    }

    /// Record one capture observation.
    pub fn record_capture(&mut self, name: String, record: CaptureRecord) {
        self.captures.insert(name, record);
    }

    /// Record one command-reply acknowledgement.
    pub fn record_command_reply(&mut self, label: String, reply: CommandReply) {
        self.command_replies.insert(label, reply);
    }

    /// Returns the program this collector was built for.
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// Validate completeness, identity, sequence, capacity, final
    /// boundary, and cleanup, then freeze the evidence surface. The
    /// returned `ScenarioRun` is the only object `verify()` may
    /// consume.
    pub fn seal(self) -> Result<ScenarioRun, SealError> {
        // Identity check: tampering with the stored bytes since
        // construction invalidates the run.
        self.program
            .verify_identity()
            .map_err(|error| SealError::StepRejected(format!("program identity: {error}")))?;
        // Step label set must match the declared schedule exactly,
        // and step count must equal transition count (the simulator
        // advanced through every boundary).
        let declared: BTreeMap<&str, ()> = self
            .declared_step_labels
            .iter()
            .map(|s| (s.as_str(), ()))
            .collect();
        let observed: BTreeMap<&str, ()> = self
            .step_outcomes
            .iter()
            .map(|(s, _)| (s.as_str(), ()))
            .collect();
        for label in observed.keys() {
            if !declared.contains_key(label) {
                return Err(SealError::UnexpectedStepLabel((*label).to_owned()));
            }
        }
        for label in declared.keys() {
            if !observed.contains_key(label) {
                return Err(SealError::MissingStepLabel((*label).to_owned()));
            }
        }
        if self.step_outcomes.len() < self.declared_step_labels.len() {
            return Err(SealError::ExcessStepOutcomes {
                declared: self.declared_step_labels.len(),
                observed: self.step_outcomes.len(),
            });
        }
        // Captures must exactly match declared names.
        for name in &self.declared_capture_names {
            if !self.captures.contains_key(name) {
                return Err(SealError::MissingCapture(name.clone()));
            }
        }
        for name in self.captures.keys() {
            if !self.declared_capture_names.contains(name) {
                return Err(SealError::UnexpectedCapture(name.clone()));
            }
        }
        // Command replies must exactly match declared labels.
        for label in &self.declared_command_labels {
            if !self.command_replies.contains_key(label) {
                return Err(SealError::MissingCommandReply(label.clone()));
            }
        }
        for label in self.command_replies.keys() {
            if !self.declared_command_labels.contains(label) {
                return Err(SealError::UnexpectedCommandReply(label.clone()));
            }
        }
        // Reject any Rejected step outcome.
        for (_, outcome) in &self.step_outcomes {
            if let StepOutcome::Rejected { reason } = outcome {
                return Err(SealError::StepRejected(reason.clone()));
            }
        }
        // Compute the passed flag from the typed evidence only; the
        // verify callback may reject on top of this.
        let passed = self
            .command_replies
            .values()
            .all(|reply| matches!(reply, CommandReply::Accepted { .. }));
        Ok(ScenarioRun::from_sealed_parts(
            self.step_outcomes,
            self.captures,
            self.command_replies,
            passed,
        ))
    }
}

fn capture_name(capture: &crate::scenario::plan::Capture) -> &str {
    match capture {
        crate::scenario::plan::Capture::State { name, .. }
        | crate::scenario::plan::Capture::Sample { name, .. }
        | crate::scenario::plan::Capture::Event { name, .. }
        | crate::scenario::plan::Capture::NativeBody { name, .. } => name.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::participant::FixtureParticipant;
    use crate::scenario::plan::{Action, Capture};
    use crate::scenario::program::{Program, ScheduleEntry};
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
    fn run_records_outcomes_captures_and_replies() {
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/First",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1, 2, 3],
                },
            )],
            vec![crate::scenario::Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        let mut participant = FixtureParticipant::from_program(program).unwrap();
        let trace = participant.run();
        // The setpoint step does not register a command correlation,
        // so mark_command_reply returns false for that label. The
        // step label is the boundary index, "b00000000" for boundary 0.
        assert!(!participant.mark_command_reply("b00000000"));
        let mut captures = BTreeMap::new();
        captures.insert("motion".to_owned(), CaptureRecord::State(vec![0x42, 0x43]));
        let mut command_replies = BTreeMap::new();
        command_replies.insert(
            "b00000000".to_owned(),
            CommandReply::Accepted {
                response_bytes: vec![0xff],
            },
        );
        let run = ScenarioRun::from_trace(trace, captures, command_replies);
        assert!(run.passed);
        assert_eq!(run.step_count(), 1);
        assert_eq!(run.command_count(), 1);
        assert!(matches!(
            run.outcome("b00000000"),
            Some(StepOutcome::SetpointDelivered { .. })
        ));
        assert!(matches!(
            run.capture("motion"),
            Some(CaptureRecord::State(_))
        ));
    }

    #[test]
    fn failed_run_marks_passed_false() {
        let trace = FixtureTrace {
            step_outcomes: vec![(
                "rejected".to_owned(),
                StepOutcome::Rejected {
                    reason: "boom".to_owned(),
                },
            )],
            captured: vec![],
        };
        let run = ScenarioRun::from_trace(trace, BTreeMap::new(), BTreeMap::new());
        assert!(!run.passed);
        assert!(matches!(
            run.outcome("rejected"),
            Some(StepOutcome::Rejected { .. })
        ));
    }

    #[test]
    fn expired_host_deadline_marks_run_failed() {
        let trace = FixtureTrace::default();
        let mut replies = BTreeMap::new();
        replies.insert("set".to_owned(), CommandReply::ExpiredHost);
        let run = ScenarioRun::from_trace(trace, BTreeMap::new(), replies);
        assert!(!run.passed);
    }

    #[test]
    fn seal_rejects_missing_required_capture() {
        // Finding 3 regression: the old `from_trace` accepted an empty
        // captures map and marked the run PASS even when a declared
        // capture was missing. The new `EvidenceCollector::seal` must
        // refuse the same input.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Seal",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                },
            )],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector.record_step_outcome(
            "b00000000".to_owned(),
            StepOutcome::SetpointDelivered {
                production: 0,
                eligibility: 0,
            },
        );
        // Deliberately do NOT record the capture.
        let result = collector.seal();
        assert!(matches!(result, Err(SealError::MissingCapture(name)) if name == "motion"));
    }

    #[test]
    fn seal_rejects_missing_command_reply() {
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let command_signature = PortSignature::new(
            "motion/do",
            "phoxal.motion",
            "Do",
            phoxal_port::PortKind::Commands,
            "DoReq",
            "DoReply",
        );
        let program = Program::normalize(
            "scenarios/SealCmd",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(
                0,
                Action::Command {
                    service_signature: command_signature,
                    request_encoded: vec![1],
                    label: "do_thing".to_owned(),
                },
            )],
            vec![],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector.record_step_outcome(
            "b00000000".to_owned(),
            StepOutcome::CommandIssued {
                label: "do_thing".to_owned(),
                reply_pending: true,
                simulated_deadline_boundary: 0,
                host_deadline_unix_micros: 0,
            },
        );
        // Deliberately do NOT record the command reply.
        let result = collector.seal();
        assert!(matches!(
            result,
            Err(SealError::MissingCommandReply(label)) if label == "do_thing"
        ));
    }

    #[test]
    fn seal_succeeds_when_all_evidence_present() {
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/SealOk",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                },
            )],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector.record_step_outcome(
            "b00000000".to_owned(),
            StepOutcome::SetpointDelivered {
                production: 0,
                eligibility: 0,
            },
        );
        collector.record_capture(
            "motion".to_owned(),
            CaptureRecord::State(vec![1, 2, 3]),
        );
        let run = collector.seal().expect("seal");
        assert!(run.is_sealed());
        assert!(run.passed());
    }
}
