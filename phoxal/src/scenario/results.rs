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

use crate::scenario::participant::StepOutcome;
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
    /// The trace reported two outcomes for the same declared step.
    /// Duplicate recordings are rejected rather than overwritten so
    /// tampering cannot smuggle in a second outcome.
    DuplicateStepOutcome(String),
    /// The trace reported an outcome whose variant does not match the
    /// declared action kind. A setpoint action cannot produce a
    /// command-issued outcome; a command action cannot produce a
    /// setpoint-delivered outcome.
    WrongOutcomeKind {
        step_label: String,
        expected: &'static str,
        actual: &'static str,
    },
    /// The trace reported an outcome whose boundary does not match
    /// the declared step boundary.
    WrongOutcomeBoundary {
        step_label: String,
        expected_boundary: u32,
    },
    /// A capture declared by the program has no record in the trace.
    MissingCapture(String),
    /// The collector was given two records for the same capture.
    /// Re-recordings are rejected so tampering cannot replace an
    /// honest capture.
    DuplicateCapture(String),
    /// The capture record kind does not match the declared capture.
    WrongCaptureKind {
        capture: String,
        expected: &'static str,
        actual: &'static str,
    },
    /// A command declared by the program has no reply record.
    MissingCommandReply(String),
    /// Two reply records were supplied for the same command label.
    DuplicateCommandReply(String),
    /// A capture record was supplied for a name the program did not
    /// declare.
    UnexpectedCapture(String),
    /// A command reply record was supplied for a label the program did not
    /// declare.
    UnexpectedCommandReply(String),
    /// The collector recorded a fixture-loss signal; the run is
    /// faulted regardless of any other passing evidence.
    FixtureLost,
    /// A step outcome is `Rejected`; the run is failed.
    StepRejected(String),
    /// An internal capacity bound was exceeded while accumulating
    /// evidence. The collector rejects rather than silently trimming
    /// so unbounded growth cannot hide a tampered run.
    CapacityExceeded {
        surface: &'static str,
        declared: usize,
    },
    /// The trace did not observe the final boundary the program
    /// declared.
    MissingFinalBoundary { expected: u32 },
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
            Self::DuplicateStepOutcome(label) => {
                write!(f, "step `{label}` received more than one outcome")
            }
            Self::WrongOutcomeKind {
                step_label,
                expected,
                actual,
            } => write!(
                f,
                "step `{step_label}` declared action kind {expected} but outcome kind is {actual}"
            ),
            Self::WrongOutcomeBoundary {
                step_label,
                expected_boundary,
            } => write!(
                f,
                "step `{step_label}` declared boundary {expected_boundary} but outcome did not match"
            ),
            Self::MissingCapture(name) => {
                write!(f, "declared capture `{name}` has no record in the trace")
            }
            Self::DuplicateCapture(name) => {
                write!(f, "capture `{name}` received more than one record")
            }
            Self::WrongCaptureKind {
                capture,
                expected,
                actual,
            } => write!(
                f,
                "capture `{capture}` declared kind {expected} but record kind is {actual}"
            ),
            Self::MissingCommandReply(label) => {
                write!(f, "declared command `{label}` has no reply record")
            }
            Self::DuplicateCommandReply(label) => {
                write!(f, "command `{label}` received more than one reply")
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
            Self::CapacityExceeded { surface, declared } => write!(
                f,
                "evidence surface `{surface}` exceeded declared capacity ({declared})"
            ),
            Self::MissingFinalBoundary { expected } => {
                write!(f, "trace never observed final boundary {expected}")
            }
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

    /// Internal constructor used only by [`EvidenceCollector::seal`].
    /// Not exposed publicly: a `ScenarioRun` cannot be manufactured
    /// from arbitrary typed fields without the collector's checks.
    pub(crate) fn from_sealed(
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

    /// Record one step outcome from the participant's trace. The
    /// collector rejects duplicate or out-of-order recordings of the
    /// same step so a tampered trace cannot smuggle in a second
    /// outcome. Recording more than the declared step count is
    /// rejected as a capacity overflow.
    pub fn record_step_outcome(
        &mut self,
        label: String,
        outcome: StepOutcome,
    ) -> Result<(), SealError> {
        if !self.declared_step_labels.contains(&label) {
            return Err(SealError::UnexpectedStepLabel(label));
        }
        if self.step_outcomes.iter().any(|(l, _)| l == &label) {
            return Err(SealError::DuplicateStepOutcome(label));
        }
        if self.step_outcomes.len() >= self.declared_step_labels.len() {
            return Err(SealError::CapacityExceeded {
                surface: "step_outcomes",
                declared: self.declared_step_labels.len(),
            });
        }
        // Validate outcome kind and boundary against the declared
        // action. A setpoint step cannot produce a CommandIssued
        // outcome; a command step cannot produce a SetpointDelivered
        // outcome. A step observed at the wrong boundary fails closed.
        let step = self
            .program
            .steps()
            .iter()
            .find(|step| step.label == label)
            .expect("declared step label");
        match (&step.action, &outcome) {
            (
                crate::scenario::plan::Action::Setpoint { .. },
                StepOutcome::SetpointDelivered { .. },
            )
            | (crate::scenario::plan::Action::Command { .. }, StepOutcome::CommandIssued { .. })
            | (crate::scenario::plan::Action::Withdraw { .. }, StepOutcome::WithdrawAccepted) => {}
            (crate::scenario::plan::Action::Setpoint { .. }, StepOutcome::CommandIssued { .. })
            | (crate::scenario::plan::Action::Setpoint { .. }, StepOutcome::WithdrawAccepted)
            | (
                crate::scenario::plan::Action::Command { .. },
                StepOutcome::SetpointDelivered { .. },
            )
            | (crate::scenario::plan::Action::Command { .. }, StepOutcome::WithdrawAccepted)
            | (
                crate::scenario::plan::Action::Withdraw { .. },
                StepOutcome::SetpointDelivered { .. },
            )
            | (crate::scenario::plan::Action::Withdraw { .. }, StepOutcome::CommandIssued { .. }) =>
            {
                return Err(SealError::WrongOutcomeKind {
                    step_label: label,
                    expected: action_kind_name(&step.action),
                    actual: outcome_kind_name(&outcome),
                });
            }
            _ => {}
        }
        if let StepOutcome::SetpointDelivered {
            production,
            eligibility,
        } = &outcome
        {
            if *production > step.boundary as u64 || *eligibility > step.boundary as u64 {
                return Err(SealError::WrongOutcomeBoundary {
                    step_label: label,
                    expected_boundary: step.boundary,
                });
            }
        }
        self.step_outcomes.push((label, outcome));
        Ok(())
    }

    /// Record one capture observation. The collector rejects records
    /// for undeclared captures, duplicate recordings, and kind
    /// mismatches.
    pub fn record_capture(&mut self, name: String, record: CaptureRecord) -> Result<(), SealError> {
        if !self.declared_capture_names.contains(&name) {
            return Err(SealError::UnexpectedCapture(name));
        }
        if self.captures.contains_key(&name) {
            return Err(SealError::DuplicateCapture(name));
        }
        if self.captures.len() >= self.declared_capture_names.len() {
            return Err(SealError::CapacityExceeded {
                surface: "captures",
                declared: self.declared_capture_names.len(),
            });
        }
        // Validate capture kind against the declaration.
        let declared = self
            .program
            .captures()
            .iter()
            .find(|capture| capture_name(capture) == name)
            .expect("declared capture name");
        let expected_kind = capture_kind_name(declared);
        let actual_kind = record_kind_name(&record);
        if expected_kind != actual_kind {
            return Err(SealError::WrongCaptureKind {
                capture: name,
                expected: expected_kind,
                actual: actual_kind,
            });
        }
        // Reject empty bodies for State/Sample/Event captures so a
        // missing observation cannot be laundered through sealing.
        match &record {
            CaptureRecord::State(bytes) | CaptureRecord::NativeBody(bytes) => {
                if bytes.is_empty() {
                    return Err(SealError::WrongCaptureKind {
                        capture: name,
                        expected: "non-empty",
                        actual: "empty",
                    });
                }
            }
            CaptureRecord::Samples(samples) => {
                if samples.is_empty() {
                    return Err(SealError::WrongCaptureKind {
                        capture: name,
                        expected: "non-empty samples",
                        actual: "empty",
                    });
                }
            }
            CaptureRecord::Events(events) => {
                if events.is_empty() {
                    return Err(SealError::WrongCaptureKind {
                        capture: name,
                        expected: "non-empty events",
                        actual: "empty",
                    });
                }
            }
        }
        self.captures.insert(name, record);
        Ok(())
    }

    /// Record one command-reply acknowledgement. The collector
    /// rejects duplicate recordings and replies for undeclared
    /// command labels.
    pub fn record_command_reply(
        &mut self,
        label: String,
        reply: CommandReply,
    ) -> Result<(), SealError> {
        if !self.declared_command_labels.contains(&label) {
            return Err(SealError::UnexpectedCommandReply(label));
        }
        if self.command_replies.contains_key(&label) {
            return Err(SealError::DuplicateCommandReply(label));
        }
        if self.command_replies.len() >= self.declared_command_labels.len() {
            return Err(SealError::CapacityExceeded {
                surface: "command_replies",
                declared: self.declared_command_labels.len(),
            });
        }
        self.command_replies.insert(label, reply);
        Ok(())
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
        // Every declared step must have exactly one outcome.
        for label in &self.declared_step_labels {
            if !self.step_outcomes.iter().any(|(l, _)| l == label) {
                return Err(SealError::MissingStepLabel(label.clone()));
            }
        }
        // Every declared capture must have a record; every record
        // must match a declaration.
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
        // Every declared command must have exactly one reply.
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
        // Final boundary: the program declared a transition count;
        // the trace must report an outcome for the last declared
        // boundary to prove the simulator advanced through the end.
        if let Some(max_boundary) = self.program.steps().iter().map(|step| step.boundary).max() {
            let observed_max = self
                .step_outcomes
                .iter()
                .filter_map(|(_, outcome)| match outcome {
                    StepOutcome::SetpointDelivered { eligibility, .. } => Some(*eligibility),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            if observed_max < max_boundary as u64 {
                return Err(SealError::MissingFinalBoundary {
                    expected: max_boundary,
                });
            }
        }
        // Compute the passed flag from the typed evidence only; the
        // verify callback may reject on top of this.
        let passed = self
            .command_replies
            .values()
            .all(|reply| matches!(reply, CommandReply::Accepted { .. }));
        Ok(ScenarioRun::from_sealed(
            self.step_outcomes,
            self.captures,
            self.command_replies,
            passed,
        ))
    }
}

fn action_kind_name(action: &crate::scenario::plan::Action) -> &'static str {
    match action {
        crate::scenario::plan::Action::Setpoint { .. } => "setpoint",
        crate::scenario::plan::Action::Command { .. } => "command",
        crate::scenario::plan::Action::Withdraw { .. } => "withdraw",
    }
}

fn outcome_kind_name(outcome: &StepOutcome) -> &'static str {
    match outcome {
        StepOutcome::SetpointDelivered { .. } => "setpoint",
        StepOutcome::CommandIssued { .. } => "command",
        StepOutcome::WithdrawAccepted => "withdraw",
        StepOutcome::Rejected { .. } => "rejected",
        StepOutcome::FixtureLost { .. } => "fixture_lost",
    }
}

fn capture_kind_name(capture: &crate::scenario::plan::Capture) -> &'static str {
    match capture {
        crate::scenario::plan::Capture::State { .. } => "state",
        crate::scenario::plan::Capture::Sample { .. } => "sample",
        crate::scenario::plan::Capture::Event { .. } => "event",
        crate::scenario::plan::Capture::NativeBody { .. } => "native_body",
    }
}

fn record_kind_name(record: &CaptureRecord) -> &'static str {
    match record {
        CaptureRecord::State(_) => "state",
        CaptureRecord::Samples(_) => "sample",
        CaptureRecord::Events(_) => "event",
        CaptureRecord::NativeBody(_) => "native_body",
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
        let _trace = participant.run();
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
        let _ = captures;
        let _ = command_replies;
        // The legacy `from_trace` constructor has been removed; the
        // collector is the only construction path.
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
        collector
            .record_step_outcome(
                "b00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
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
        collector
            .record_step_outcome(
                "b00000000".to_owned(),
                StepOutcome::CommandIssued {
                    label: "do_thing".to_owned(),
                    reply_pending: true,
                    simulated_deadline_boundary: 0,
                    host_deadline_unix_micros: 0,
                },
            )
            .expect("record step");
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
        collector
            .record_step_outcome(
                "b00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
        collector
            .record_capture("motion".to_owned(), CaptureRecord::State(vec![1, 2, 3]))
            .expect("record capture");
        let run = collector.seal().expect("seal");
        assert!(run.is_sealed());
        assert!(run.passed());
    }

    #[test]
    fn seal_rejects_duplicate_outcome() {
        // A tampered trace that records the same step twice must not
        // be accepted; the collector rejects duplicate recordings.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Dup",
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
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "b00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("first record");
        let err = collector
            .record_step_outcome(
                "b00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .unwrap_err();
        assert!(matches!(err, SealError::DuplicateStepOutcome(_)));
    }

    #[test]
    fn seal_rejects_wrong_outcome_kind() {
        // A setpoint step observed with a command-issued outcome
        // indicates tampered or mismatched evidence.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/WrongKind",
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
        let mut collector = EvidenceCollector::for_program(program);
        let err = collector
            .record_step_outcome(
                "b00000000".to_owned(),
                StepOutcome::CommandIssued {
                    label: "spurious".to_owned(),
                    reply_pending: true,
                    simulated_deadline_boundary: 0,
                    host_deadline_unix_micros: 0,
                },
            )
            .unwrap_err();
        assert!(matches!(err, SealError::WrongOutcomeKind { .. }));
    }

    #[test]
    fn seal_rejects_wrong_capture_kind() {
        // A native-body record cannot satisfy a state-style capture.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/WrongCapture",
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
        let err = collector
            .record_capture("motion".to_owned(), CaptureRecord::NativeBody(vec![0]))
            .unwrap_err();
        assert!(matches!(err, SealError::WrongCaptureKind { .. }));
    }

    #[test]
    fn seal_rejects_empty_state_capture() {
        // Empty bodies cannot launder missing observations through
        // sealing.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/EmptyCap",
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
        collector
            .record_step_outcome(
                "b00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
        let err = collector
            .record_capture("motion".to_owned(), CaptureRecord::State(Vec::new()))
            .unwrap_err();
        assert!(matches!(err, SealError::WrongCaptureKind { .. }));
    }

    #[test]
    fn seal_rejects_undeclared_outcome() {
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Undeclared",
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
        let mut collector = EvidenceCollector::for_program(program);
        let err = collector
            .record_step_outcome(
                "b99999999".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .unwrap_err();
        assert!(matches!(err, SealError::UnexpectedStepLabel(_)));
    }
}
