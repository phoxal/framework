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
#[cfg(test)]
use crate::scenario::program::ProgramError;

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
    /// A `FixtureLost` outcome was reported; the run is refused
    /// outright and the recorded reason is returned. Distinct from
    /// `FixtureLost` because this variant carries the recorded
    /// reason string.
    FixtureLostReported(String),
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
    /// A single capture record exceeds `MAX_RECORD_BYTES`.
    CaptureByteOverflow {
        capture: String,
        bytes: usize,
        cap: usize,
    },
    /// A single command reply payload exceeds `MAX_RECORD_BYTES`.
    ReplyByteOverflow {
        label: String,
        bytes: usize,
        cap: usize,
    },
    /// An interval capture (Samples / Events) has more entries than
    /// `MAX_INTERVAL_ENTRIES`.
    CaptureEntryOverflow {
        capture: String,
        entries: usize,
        cap: usize,
    },
    /// Cumulative bytes across all capture records in the run
    /// exceed `MAX_RUN_BYTES`.
    RunByteOverflow { bytes: usize, cap: usize },
    /// The seal was called without terminal evidence recorded by
    /// the actual execution lifecycle. See Gate P1 #2 of
    /// the scenario acceptance review.
    MissingTerminalEvidence,
    /// The seal refused to record a second terminal-evidence call.
    DuplicateTerminalEvidence,
    /// The lifecycle's terminal-evidence final observation cut was
    /// not observed.
    MissingFinalObservationCut,
    /// The lifecycle's terminal-evidence final capture drain was
    /// not observed.
    MissingFinalCaptureDrain,
    /// The lifecycle's terminal-evidence cleanup did not succeed.
    CleanupFailed,
    /// The terminal evidence quantum (ns) does not equal the
    /// admitted program quantum.
    TerminalQuantumMismatch { declared: u64, program: u64 },
    /// The terminal evidence completed-transition count does not
    /// equal `program.transition_count()`.
    TerminalCompletionMismatch { completed: u64, required: u64 },
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
            Self::FixtureLostReported(reason) => {
                write!(f, "fixture lost during execution: {reason}")
            }
            Self::StepRejected(reason) => write!(f, "step was rejected: {reason}"),
            Self::CapacityExceeded { surface, declared } => write!(
                f,
                "evidence surface `{surface}` exceeded declared capacity ({declared})"
            ),
            Self::MissingFinalBoundary { expected } => {
                write!(f, "trace never observed final boundary {expected}")
            }
            Self::MissingTerminalEvidence => write!(
                f,
                "terminal evidence has not been recorded by the execution lifecycle; \
                 the collector cannot synthesize it"
            ),
            Self::DuplicateTerminalEvidence => {
                write!(f, "terminal evidence was already recorded for this run")
            }
            Self::MissingFinalObservationCut => write!(
                f,
                "terminal evidence reports the final observation cut never completed"
            ),
            Self::MissingFinalCaptureDrain => write!(
                f,
                "terminal evidence reports the final capture drain never completed"
            ),
            Self::CleanupFailed => write!(f, "terminal evidence reports cleanup failed"),
            Self::TerminalQuantumMismatch { declared, program } => write!(
                f,
                "terminal evidence quantum {declared} ns does not match program quantum {program} ns"
            ),
            Self::TerminalCompletionMismatch {
                completed,
                required,
            } => write!(
                f,
                "terminal evidence completed {completed} transitions; the program requires {required}"
            ),
            Self::CaptureByteOverflow {
                capture,
                bytes,
                cap,
            } => write!(
                f,
                "capture `{capture}` is {bytes} bytes, exceeding the {cap}-byte per-record cap"
            ),
            Self::ReplyByteOverflow { label, bytes, cap } => write!(
                f,
                "command reply `{label}` is {bytes} bytes, exceeding the {cap}-byte per-record cap"
            ),
            Self::CaptureEntryOverflow {
                capture,
                entries,
                cap,
            } => write!(
                f,
                "capture `{capture}` has {entries} entries, exceeding the {cap}-entry interval cap"
            ),
            Self::RunByteOverflow { bytes, cap } => {
                write!(f, "cumulative capture bytes {bytes} exceed run cap {cap}")
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
    /// Terminal evidence recorded by the actual execution lifecycle.
    /// `seal` refuses to finalize without it; the four collector
    /// reproduction probes from the scenario acceptance review all fail
    /// because they cannot synthesize this surface. See Gate P1 #2
    /// of that review.
    terminal_evidence: Option<TerminalEvidence>,
    /// Whether the host considers the run successful. Computed by
    /// `seal`; the `verify` callback may reject on top of this.
    passed: bool,
    /// `true` once the collector has frozen the evidence surface.
    /// Only sealed runs reach `verify`.
    sealed: bool,
}

/// Terminal evidence the actual execution lifecycle records once.
/// Provenance, validation, final observation cut, and cleanup
/// ownership are part of the case-host lifecycle. The collector
/// cannot synthesize this surface itself.
///
/// Fields are private; construction goes through
/// [`EvidenceCollector::terminal_evidence_builder`] which fills the
/// structural fields from the collector's program and tracks the
/// lifecycle flags separately. External code cannot assemble a
/// `TerminalEvidence` from arbitrary fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalEvidence {
    /// Identity of the execution that produced this evidence.
    execution_identity: String,
    /// Exact native quantum in nanoseconds.
    quantum_ns: u64,
    /// Number of native transitions the experiment ran.
    completed_transitions: u64,
    /// Final observation cut observed by the case host.
    final_observation_cut: bool,
    /// Final capture drain observed by the case host.
    final_capture_drain: bool,
    /// Cleanup outcome.
    cleanup_ok: bool,
}

impl TerminalEvidence {
    /// Identity of the execution that produced this evidence.
    #[must_use]
    pub fn execution_identity(&self) -> &str {
        &self.execution_identity
    }

    /// Exact native quantum in nanoseconds.
    #[must_use]
    pub const fn quantum_ns(&self) -> u64 {
        self.quantum_ns
    }

    /// Number of native transitions the experiment ran.
    #[must_use]
    pub const fn completed_transitions(&self) -> u64 {
        self.completed_transitions
    }

    /// Final observation cut observed by the case host. Until the
    /// case-host lifecycle flips this via the builder, the value
    /// is `false` and `seal` refuses.
    #[must_use]
    pub const fn final_observation_cut(&self) -> bool {
        self.final_observation_cut
    }

    /// Final capture drain observed by the case host.
    #[must_use]
    pub const fn final_capture_drain(&self) -> bool {
        self.final_capture_drain
    }

    /// Cleanup outcome.
    #[must_use]
    pub const fn cleanup_ok(&self) -> bool {
        self.cleanup_ok
    }
}

/// Builder for [`TerminalEvidence`]. Construction goes through
/// [`EvidenceCollector::terminal_evidence_builder`] so external code
/// cannot assemble a [`TerminalEvidence`] from arbitrary fields.
///
/// The structural fields (`quantum_ns`, `completed_transitions`) come
/// from the case-host lifecycle, **not** from the program. The
/// builder must be called with `with_terminal_quantum_ns` and
/// `with_completed_transitions`; absent those calls, the
/// corresponding fields default to `0`. The lifecycle flags
/// (`final_observation_cut`, `final_capture_drain`, `cleanup_ok`)
/// default to `false` and must be flipped via `*_observed` /
/// `*_drained` / `cleanup_succeeded`. The seal then validates the
/// lifecycle-observed quantum and completed-transition count against
/// the program: a quantum mismatch returns
/// [`SealError::TerminalQuantumMismatch`] and a completed-transition
/// mismatch returns [`SealError::TerminalCompletionMismatch`]. Until
/// all three lifecycle flags are set, `seal` refuses with
/// [`SealError::MissingFinalObservationCut`] /
/// [`SealError::MissingFinalCaptureDrain`] / [`SealError::CleanupFailed`].
///
/// The supervisor-driven lifecycle calls `with_terminal_quantum_ns` with the
/// simulator's observed `quantum_ns`, and `with_completed_transitions` with
/// the actual completed native transition boundary.
/// Test-only fixtures that exercise the seal surface provide these from the
/// program's known values; the public command path seals only supervisor and
/// simulator evidence through the generated case host.
pub struct TerminalEvidenceBuilder {
    execution_identity: String,
    /// Lifecycle-observed quantum in nanoseconds. The supervisor
    /// reports this from the simulator's actual probe, not from the
    /// program's declared quantum.
    terminal_quantum_ns: Option<u64>,
    /// Lifecycle-observed completed transition count. The supervisor
    /// reports this from the actual native execution, not from the
    /// program's `transition_count`.
    completed_transitions: Option<u64>,
    final_observation_cut: bool,
    final_capture_drain: bool,
    cleanup_ok: bool,
}

impl TerminalEvidenceBuilder {
    /// Set the execution identity. The collector does not validate
    /// the identity string itself; admission policy enforces a
    /// match against the bundle identity when `seal` is called.
    #[must_use]
    pub fn with_execution_identity(mut self, identity: impl Into<String>) -> Self {
        self.execution_identity = identity.into();
        self
    }

    /// Set the lifecycle-observed quantum in nanoseconds. The
    /// supervisor-driven lifecycle calls this with the value the
    /// simulator probed; test-only fixtures that exercise the seal
    /// surface may call this with the program's quantum (only the
    /// seal-time mismatch check then accepts the value). Absent
    /// this call, the field defaults to `0` and the seal reports a
    /// [`SealError::TerminalQuantumMismatch`] for any program whose
    /// quantum is non-zero.
    #[must_use]
    pub fn with_terminal_quantum_ns(mut self, quantum_ns: u64) -> Self {
        self.terminal_quantum_ns = Some(quantum_ns);
        self
    }

    /// Set the lifecycle-observed completed transition count. The
    /// supervisor-driven lifecycle calls this with the actual
    /// completed native transition boundary; test-only fixtures
    /// that exercise the seal surface may call this with the
    /// program's `transition_count`. Absent this call, the field
    /// defaults to `0` and the seal reports a
    /// [`SealError::TerminalCompletionMismatch`] for any program
    /// whose `transition_count` is non-zero.
    #[must_use]
    pub fn with_completed_transitions(mut self, completed: u64) -> Self {
        self.completed_transitions = Some(completed);
        self
    }

    /// Mark that the final observation cut was observed by the
    /// case host. Without this call, `seal` refuses.
    #[must_use]
    pub fn final_observation_cut_observed(mut self) -> Self {
        self.final_observation_cut = true;
        self
    }

    /// Mark that the final capture drain was observed.
    #[must_use]
    pub fn final_capture_drain_observed(mut self) -> Self {
        self.final_capture_drain = true;
        self
    }

    /// Mark that cleanup succeeded.
    #[must_use]
    pub fn cleanup_succeeded(mut self) -> Self {
        self.cleanup_ok = true;
        self
    }

    /// Build the [`TerminalEvidence`]. The structural fields come
    /// from the lifecycle-observed values supplied to the builder;
    /// absent those calls, the fields default to `0`. The seal
    /// validates them against the program and refuses any mismatch.
    /// Returns the evidence so the caller can hand it to
    /// [`EvidenceCollector::record_terminal_evidence`].
    #[must_use]
    pub fn build(self) -> TerminalEvidence {
        TerminalEvidence {
            execution_identity: self.execution_identity,
            quantum_ns: self.terminal_quantum_ns.unwrap_or(0),
            completed_transitions: self.completed_transitions.unwrap_or(0),
            final_observation_cut: self.final_observation_cut,
            final_capture_drain: self.final_capture_drain,
            cleanup_ok: self.cleanup_ok,
        }
    }
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

    /// Terminal evidence recorded by the execution lifecycle, if any.
    pub fn terminal_evidence(&self) -> Option<&TerminalEvidence> {
        self.terminal_evidence.as_ref()
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
        terminal_evidence: Option<TerminalEvidence>,
        passed: bool,
    ) -> Self {
        Self {
            step_outcomes,
            captures,
            command_replies,
            terminal_evidence,
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

/// Per-record byte cap. Each individual `Vec<u8>` inside a record is
/// bounded to this many bytes before the record is admitted.
pub const MAX_RECORD_BYTES: usize = 1024 * 1024;

/// Maximum number of samples/events in a single interval record.
pub const MAX_INTERVAL_ENTRIES: usize = 4096;

/// Maximum total bytes across all admitted evidence records in one
/// run. Cumulative accounting protects against unbounded growth.
pub const MAX_RUN_BYTES: usize = 16 * 1024 * 1024;

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
    /// Cumulative bytes admitted across all capture records. Bounded
    /// by `MAX_RUN_BYTES` so unbounded growth cannot hide a tampered
    /// run.
    record_bytes_total: usize,
    /// Terminal evidence recorded by the actual execution lifecycle.
    /// `seal` refuses without it; `record_terminal_evidence` is the
    /// only path that can install this field. See Gate P1 #2 of
    /// the scenario acceptance review.
    terminal_evidence: Option<TerminalEvidence>,
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
            record_bytes_total: 0,
            terminal_evidence: None,
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
        let step = match self.program.steps().iter().find(|step| step.label == label) {
            Some(step) => step,
            None => return Err(SealError::UnexpectedStepLabel(label)),
        };
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
            && (*production > step.boundary as u64
                || *eligibility > (step.boundary as u64).saturating_add(1))
        {
            // Production at the action's boundary is the only
            // acceptable producer tick. Eligibility at the next
            // boundary is the controlled-runtime receiver admission
            // rule: the receiver must be admitted within one boundary
            // of production. Production at N + eligibility at N+1 is
            // the canonical happy path; later eligibility is a
            // delayed acknowledgement that the run cannot finalize
            // inside its budget.
            return Err(SealError::WrongOutcomeBoundary {
                step_label: label,
                expected_boundary: step.boundary,
            });
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
        // Validate capture kind against the declaration. A capture
        // that the program did not declare is rejected here; the
        // prior `declared_capture_names.contains` check is the
        // source of truth, but the lookup is performed here so the
        // declared descriptor is available for kind validation.
        let declared = match self
            .program
            .captures()
            .iter()
            .find(|capture| capture_name(capture) == name)
        {
            Some(declared) => declared,
            None => return Err(SealError::UnexpectedCapture(name)),
        };
        let expected_kind = capture_kind_name(declared);
        let actual_kind = record_kind_name(&record);
        if expected_kind != actual_kind {
            return Err(SealError::WrongCaptureKind {
                capture: name,
                expected: expected_kind,
                actual: actual_kind,
            });
        }
        // Enforce per-record byte and entry caps. Each individual
        // record is bounded to `MAX_RECORD_BYTES`; interval records
        // additionally bound their entry count. Checked arithmetic
        // prevents silent truncation when summing bytes.
        let record_bytes: usize = match &record {
            CaptureRecord::State(bytes) | CaptureRecord::NativeBody(bytes) => bytes.len(),
            CaptureRecord::Samples(samples) | CaptureRecord::Events(samples) => {
                if samples.len() > MAX_INTERVAL_ENTRIES {
                    return Err(SealError::CaptureEntryOverflow {
                        capture: name,
                        entries: samples.len(),
                        cap: MAX_INTERVAL_ENTRIES,
                    });
                }
                samples
                    .iter()
                    .fold(0usize, |acc, sample| acc.saturating_add(sample.len()))
            }
        };
        if record_bytes > MAX_RECORD_BYTES {
            return Err(SealError::CaptureByteOverflow {
                capture: name,
                bytes: record_bytes,
                cap: MAX_RECORD_BYTES,
            });
        }
        self.record_bytes_total = self.record_bytes_total.saturating_add(record_bytes);
        if self.record_bytes_total > MAX_RUN_BYTES {
            return Err(SealError::RunByteOverflow {
                bytes: self.record_bytes_total,
                cap: MAX_RUN_BYTES,
            });
        }
        // Reject empty bodies for State captures so a missing
        // observation cannot be laundered through sealing. Samples
        // and Events intervals may legitimately be empty — the
        // simulator observed the window and no events fired, which
        // is a valid observation.
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
            CaptureRecord::Samples(_) | CaptureRecord::Events(_) => {}
        }
        self.captures.insert(name, record);
        Ok(())
    }

    /// Record one command-reply acknowledgement. The collector
    /// rejects duplicate recordings, replies for undeclared command
    /// labels, and reply payloads that exceed the per-record or
    /// cumulative run byte caps. See Gate B3 of
    /// the scenario acceptance review: "Account for every retained payload,
    /// including command replies, step error details, capture
    /// metadata, and nested interval entries."
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
        // Per-reply byte accounting. The reply either carries an
        // accepted response payload or an enum variant; we count
        // every byte that lands in the sealed run.
        let reply_bytes = match &reply {
            CommandReply::Accepted { response_bytes } => response_bytes.len(),
            CommandReply::ExpiredSimulated | CommandReply::ExpiredHost => 0,
            CommandReply::Rejected { reason } => reason.len(),
        };
        if reply_bytes > MAX_RECORD_BYTES {
            return Err(SealError::ReplyByteOverflow {
                label: label.clone(),
                bytes: reply_bytes,
                cap: MAX_RECORD_BYTES,
            });
        }
        let updated_total =
            self.record_bytes_total
                .checked_add(reply_bytes)
                .ok_or(SealError::RunByteOverflow {
                    bytes: reply_bytes,
                    cap: MAX_RUN_BYTES,
                })?;
        if updated_total > MAX_RUN_BYTES {
            return Err(SealError::RunByteOverflow {
                bytes: reply_bytes,
                cap: MAX_RUN_BYTES,
            });
        }
        self.record_bytes_total = updated_total;
        self.command_replies.insert(label, reply);
        Ok(())
    }

    /// Record the terminal evidence the actual execution lifecycle
    /// produces. The collector refuses a second recording; the case
    /// host records this exactly once after the experiment has
    /// actually completed through the final native transition,
    /// final observation cut, and final capture drain, and only
    /// after every owned child was reaped and every borrowed
    /// simulator released.
    ///
    /// `seal` will refuse without terminal evidence. The four
    /// collector reproduction probes from the scenario acceptance review all
    /// fail because they cannot call this method. See Gate P1 #2
    /// of that review.
    pub fn record_terminal_evidence(
        &mut self,
        evidence: TerminalEvidence,
    ) -> Result<(), SealError> {
        if self.terminal_evidence.is_some() {
            return Err(SealError::DuplicateTerminalEvidence);
        }
        if !evidence.final_observation_cut {
            return Err(SealError::MissingFinalObservationCut);
        }
        if !evidence.final_capture_drain {
            return Err(SealError::MissingFinalCaptureDrain);
        }
        if !evidence.cleanup_ok {
            return Err(SealError::CleanupFailed);
        }
        self.terminal_evidence = Some(evidence);
        Ok(())
    }

    /// Returns a `TerminalEvidenceBuilder` that owns the
    /// lifecycle-observed terminal facts. The structural fields
    /// (`quantum_ns`, `completed_transitions`) are **not** filled
    /// from this collector's program; the case-host lifecycle must
    /// observe them from the supervisor / native execution and call
    /// `with_terminal_quantum_ns` / `with_completed_transitions`.
    /// The builder is the only path that constructs
    /// [`TerminalEvidence`]; external code cannot assemble the
    /// surface from arbitrary fields. See Gate P1 #2 of
    /// the scenario acceptance review.
    pub fn terminal_evidence_builder(&mut self) -> TerminalEvidenceBuilder {
        TerminalEvidenceBuilder {
            execution_identity: String::new(),
            terminal_quantum_ns: None,
            completed_transitions: None,
            final_observation_cut: false,
            final_capture_drain: false,
            cleanup_ok: false,
        }
    }

    /// Returns the program this collector was built for.
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// Validate completeness, identity, sequence, capacity, final
    /// boundary, and cleanup, then freeze the evidence surface. The
    /// returned `ScenarioRun` is the only object `verify()` may
    /// consume.
    pub fn seal(mut self) -> Result<ScenarioRun, SealError> {
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
        // Reject any Rejected or FixtureLost outcome. Fixture loss is
        // a terminal failure that the run cannot recover from; the
        // sealed run reports it explicitly instead of letting the
        // verdict derive from completed-by-accident evidence.
        for (_, outcome) in &self.step_outcomes {
            match outcome {
                StepOutcome::Rejected { reason } => {
                    return Err(SealError::StepRejected(reason.clone()));
                }
                StepOutcome::FixtureLost { reason } => {
                    return Err(SealError::FixtureLostReported(reason.clone()));
                }
                _ => {}
            }
        }
        // Terminal evidence gate: the actual execution lifecycle owns this
        // surface. The collector cannot synthesize it; tests that call
        // the existing record_* helpers without going through the
        // lifecycle will seal as failed. See Gate P1 #2 of
        // the scenario acceptance review: the four collector reproduction probes
        // fail here.
        let terminal = self
            .terminal_evidence
            .take()
            .ok_or(SealError::MissingTerminalEvidence)?;
        // Identity check: terminal evidence must belong to the
        // admitted program.
        if terminal.quantum_ns != u64::from(self.program.quantum().micros()) * 1_000 {
            return Err(SealError::TerminalQuantumMismatch {
                declared: terminal.quantum_ns,
                program: u64::from(self.program.quantum().micros()) * 1_000,
            });
        }
        // Final-boundary check: the lifecycle reports the exact
        // completed transition count. For a finite schedule the
        // experiment completes at `transition_count` native transitions,
        // not at the maximum authored action boundary or the last
        // transition index.
        if terminal.completed_transitions != u64::from(self.program.transition_count()) {
            return Err(SealError::TerminalCompletionMismatch {
                completed: terminal.completed_transitions,
                required: u64::from(self.program.transition_count()),
            });
        }
        // The lifecycle must report successful final observation
        // cut, capture drain, and cleanup. record_terminal_evidence
        // already refused to record anything less, but a manual
        // constructor could still bypass; the seal-time check makes
        // the contract explicit.
        if !terminal.final_observation_cut {
            return Err(SealError::MissingFinalObservationCut);
        }
        if !terminal.final_capture_drain {
            return Err(SealError::MissingFinalCaptureDrain);
        }
        if !terminal.cleanup_ok {
            return Err(SealError::CleanupFailed);
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
            Some(terminal),
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
    use crate::scenario::plan::{Action, Capture, Validity};
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

    fn event_sig() -> PortSignature {
        PortSignature::new(
            "motion/event",
            "phoxal.motion",
            "Event",
            phoxal_port::PortKind::Event,
            "Event",
            "Event",
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

    fn command_signature() -> PortSignature {
        PortSignature::new(
            "motion/do",
            "phoxal.motion",
            "Do",
            phoxal_port::PortKind::Commands,
            "DoReq",
            "DoReply",
        )
    }

    fn command_action(label: &str, byte: u8) -> Action {
        Action::command(
            "motion_target",
            command_signature(),
            vec![byte],
            label,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1),
        )
        .expect("command action")
    }

    /// Build valid terminal evidence for a program. The collector's
    /// `seal` rejects evidence whose quantum does not match the
    /// program's quantum in nanoseconds, whose completed transition
    /// count does not equal `transition_count`, or whose final
    /// observation cut / capture drain / cleanup did not succeed.
    /// This helper mirrors what the actual case-host lifecycle would
    /// record after the experiment completed through the final
    /// native transition, captured the final observation, drained
    /// the capture buffer, and reaped every owned child.
    /// Records valid terminal evidence into the collector via the
    /// builder API. Used by tests that bypass the case-host
    /// lifecycle to exercise the seal path with a synthetic
    /// evidence surface. Production code reaches the builder via
    /// the case-host lifecycle, not this helper.
    fn record_valid_terminal_evidence(collector: &mut EvidenceCollector, execution_identity: &str) {
        let program_quantum_ns = u64::from(collector.program().quantum().micros()) * 1_000;
        let program_transition_count = u64::from(collector.program().transition_count());
        let evidence = collector
            .terminal_evidence_builder()
            .with_execution_identity(execution_identity)
            .with_terminal_quantum_ns(program_quantum_ns)
            .with_completed_transitions(program_transition_count)
            .final_observation_cut_observed()
            .final_capture_drain_observed()
            .cleanup_succeeded()
            .build();
        collector
            .record_terminal_evidence(evidence)
            .expect("terminal evidence must be accepted; the seal is where validity is enforced");
    }

    #[test]
    fn run_records_outcomes_captures_and_replies() {
        // Gate B1 of the scenario acceptance review removed the synthetic step
        // emitter and `FixtureTrace`. The collector-only construction
        // path is exercised by the sealed scenario run path elsewhere
        // in this module; this test now asserts the typed correlation
        // registry invariant (no run was performed, so the correlation
        // map is empty for a freshly-prepared participant).
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/First",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![crate::scenario::Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        let mut participant = FixtureParticipant::from_program(program).unwrap();
        // Setpoint-only participant: no commands issued, so mark_command_reply
        // returns false for every label.
        assert!(!participant.mark_command_reply("s00000000"));
        assert!(participant.expire_pending_commands().is_empty());
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
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
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
        let program = Program::normalize(
            "scenarios/SealCmd",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(0, command_action("do_thing", 1))],
            vec![],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
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
        // Multi-transition positive case: 3 transitions at the 2 ms
        // quantum = 6 ms plan, with a single setpoint at boundary 0.
        // The recorded lifecycle reports 3 completed transitions and
        // 2_000_000 ns quantum, matching the program. The seal then
        // accepts the sparse schedule because the lifecycle vouches
        // for the boundary `N` and the final observation cut / capture
        // drain / cleanup. Reducing the duration to one transition
        // would not exercise the N>1 boundary check; this test
        // deliberately keeps the duration multi-transition.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/SealOk",
            quantum,
            // 6 ms at the 2 ms quantum = 3 transitions; the setpoint
            // acknowledgement at boundary 0 is the only authored
            // action. See Gate B2 and Gate P1 #2.
            std::time::Duration::from_micros(6_000),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
        collector
            .record_capture("motion".to_owned(), CaptureRecord::State(vec![1, 2, 3]))
            .expect("record capture");
        record_valid_terminal_evidence(&mut collector, "exec/scenario/seal_ok");
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
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("first record");
        let err = collector
            .record_step_outcome(
                "s00000000".to_owned(),
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
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        let err = collector
            .record_step_outcome(
                "s00000000".to_owned(),
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
            vec![ScheduleEntry::at(0, setpoint_action(1))],
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
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
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
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        let err = collector
            .record_step_outcome(
                "s99999999".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .unwrap_err();
        assert!(matches!(err, SealError::UnexpectedStepLabel(_)));
    }

    #[test]
    fn seal_accepts_production_at_n_eligibility_at_n_plus_one() {
        // Controlled-runtime happy path: producer at boundary N,
        // consumer admitted at boundary N+1.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/NToNPlusOne",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 1,
                },
            )
            .expect("N-to-N+1 must be accepted");
    }

    #[test]
    fn seal_rejects_eligibility_at_n_plus_two() {
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/StaleAck",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        let err = collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 2,
                },
            )
            .unwrap_err();
        assert!(matches!(err, SealError::WrongOutcomeBoundary { .. }));
    }

    #[test]
    fn seal_rejects_fixture_lost_outcome() {
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/FixtureLost",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::FixtureLost {
                    reason: "fixture child died".to_owned(),
                },
            )
            .expect("recording fixture loss");
        let err = collector.seal().unwrap_err();
        assert!(matches!(err, SealError::FixtureLostReported(_)));
    }

    #[test]
    fn seal_finalizes_command_only_program_without_setpoint_boundary() {
        // A program that has only Command actions has no setpoint
        // boundary to observe; the final-cut check must finalize
        // through command replies plus real terminal evidence.
        // 3 transitions at the 2 ms quantum = 6 ms plan; the
        // setpoint-eligibility heuristic is intentionally absent
        // here so the seal must lean on terminal evidence alone.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/CommandOnly",
            quantum,
            std::time::Duration::from_micros(6_000),
            vec![ScheduleEntry::at(0, command_action("do_thing", 1))],
            vec![],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::CommandIssued {
                    label: "do_thing".to_owned(),
                    reply_pending: true,
                    simulated_deadline_boundary: 0,
                    host_deadline_unix_micros: 0,
                },
            )
            .expect("record command");
        collector
            .record_command_reply(
                "do_thing".to_owned(),
                CommandReply::Accepted {
                    response_bytes: vec![0xff],
                },
            )
            .expect("record reply");
        record_valid_terminal_evidence(&mut collector, "exec/scenario/command_only");
        let run = collector.seal().expect("seal");
        assert!(run.is_sealed());
        assert!(run.passed());
    }

    #[test]
    fn seal_rejects_oversized_capture_record() {
        // Per-record byte cap: a 2 MiB state record must be refused.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/OversizedCapture",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
        let big = vec![0u8; 2 * 1024 * 1024];
        let err = collector
            .record_capture("motion".to_owned(), CaptureRecord::State(big))
            .unwrap_err();
        assert!(matches!(
            err,
            SealError::CaptureByteOverflow {
                cap: super::MAX_RECORD_BYTES,
                ..
            }
        ));
    }

    #[test]
    fn seal_rejects_oversized_event_interval() {
        // Interval entry cap: more than MAX_INTERVAL_ENTRIES events
        // must be refused.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/OversizedInterval",
            quantum,
            std::time::Duration::from_secs(1),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::event("motion", event_sig()).expect("event capture")],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
        let too_many = vec![vec![1u8]; super::MAX_INTERVAL_ENTRIES + 1];
        let err = collector
            .record_capture("motion".to_owned(), CaptureRecord::Events(too_many))
            .unwrap_err();
        assert!(matches!(
            err,
            SealError::CaptureEntryOverflow {
                cap: super::MAX_INTERVAL_ENTRIES,
                ..
            }
        ));
    }

    #[test]
    fn seal_accepts_zero_event_interval() {
        // An empty event interval is a legitimate observation:
        // the simulator saw the window, no events fired. The
        // collector must accept an empty Samples/Events record
        // (the validation is on presence, not on payload length).
        // The 6 ms / 2 ms quantum program has 3 transitions; the
        // single setpoint acknowledgement at boundary 0 is the only
        // authored action. Terminal evidence records all 3 transitions
        // so the seal accepts.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/ZeroEventInterval",
            quantum,
            std::time::Duration::from_micros(6_000),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::event("motion", event_sig()).expect("event capture")],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
        collector
            .record_capture("motion".to_owned(), CaptureRecord::Events(Vec::new()))
            .expect("zero-event interval must be accepted");
        record_valid_terminal_evidence(&mut collector, "exec/scenario/zero_event_interval");
        let run = collector.seal().expect("seal");
        assert!(run.is_sealed());
    }

    #[test]
    fn seal_rejects_3000_transition_plan_with_only_boundary_zero() {
        // Regression for the scenario acceptance review line 333: "3000-transition
        // program seals with only boundary zero: Ok(true)". A
        // 6-second plan with only a single setpoint at boundary 0
        // cannot finalize because the experiment did not run. With
        // the new design the collector refuses at the
        // MissingTerminalEvidence gate: this test does not invoke
        // the lifecycle, so the lifecycle never recorded
        // `completed_transitions = 3000`, the final observation cut,
        // the final capture drain, or the cleanup outcome. A caller
        // that fabricated those would still fail at the quantum /
        // completion / cut checks; see the dedicated regressions
        // below.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/LongSparse",
            quantum,
            std::time::Duration::from_secs(6),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
        collector
            .record_capture("motion".to_owned(), CaptureRecord::State(vec![0xAA]))
            .expect("record capture");
        let err = collector
            .seal()
            .expect_err("3000-transition plan with boundary-zero evidence must not seal");
        assert!(
            matches!(err, SealError::MissingTerminalEvidence),
            "collector without lifecycle must refuse at MissingTerminalEvidence; got {err:?}"
        );
    }

    #[test]
    fn seal_rejects_oversized_command_reply_payload() {
        // Regression for the scenario acceptance review line 336: "duplicate
        // commands + wrong occurrence + 17MiB reply seal: Ok(true)".
        // A command reply whose payload exceeds MAX_RECORD_BYTES
        // must be refused at record time, never sealed as passed.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/OversizedReply",
            quantum,
            std::time::Duration::from_micros(2_000),
            vec![ScheduleEntry::at(0, command_action("do_thing", 1))],
            vec![],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        // Record the step outcome so the seal reaches the command
        // reply check (otherwise MissingStepLabel fires first).
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::CommandIssued {
                    label: "do_thing".to_owned(),
                    reply_pending: true,
                    simulated_deadline_boundary: 0,
                    host_deadline_unix_micros: 0,
                },
            )
            .expect("record step");
        let oversized = vec![0u8; super::MAX_RECORD_BYTES + 1];
        let err = collector
            .record_command_reply(
                "do_thing".to_owned(),
                CommandReply::Accepted {
                    response_bytes: oversized.clone(),
                },
            )
            .expect_err("oversized reply payload must be refused at record time");
        assert!(matches!(
            err,
            SealError::ReplyByteOverflow {
                ref label,
                bytes,
                cap,
            } if label == "do_thing" && bytes == super::MAX_RECORD_BYTES + 1 && cap == super::MAX_RECORD_BYTES
        ));
        // Sealing after the rejection must fail (the collector is
        // still missing the reply, so MissingCommandReply is the
        // diagnostic).
        let err = collector.seal().expect_err("seal must refuse");
        match err {
            SealError::MissingCommandReply(_) => {}
            other => panic!("expected MissingCommandReply, got {other:?}"),
        }
    }

    #[test]
    fn seal_rejects_duplicate_command_label_pair() {
        // Regression for the scenario acceptance review line 336: duplicate
        // commands with the same label. The plan-time validator must
        // refuse this before any collector is built so the test
        // cannot reach the seal path.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let result = Program::normalize(
            "scenarios/DupCommands",
            quantum,
            std::time::Duration::from_micros(2_000),
            vec![
                ScheduleEntry::at(0, command_action("cmd", 1)),
                ScheduleEntry::at(0, command_action("cmd", 2)),
            ],
            vec![],
        );
        assert!(matches!(result, Err(ProgramError::Other(_))));
    }

    // ----------------------------------------------------------------
    // Gate P1 #2 of the scenario acceptance review: the four reproduction probes
    // from lines 166-173 plus the boundary / interruption / mismatch
    // / sparse-valid regressions required by line 195. These tests
    // exercise the public collector API directly, without going
    // through the case-host lifecycle, so terminal evidence is not
    // recorded and the seal must refuse.
    // ----------------------------------------------------------------

    #[test]
    fn reproduction_no_action_3000_transition_run_refuses_without_terminal_evidence() {
        // Probe 1 from the scenario acceptance review line 169: "no-action
        // 3000-transition run without execution seals: Ok(true)".
        // An empty schedule of 3000 transitions cannot finalize;
        // the lifecycle never recorded any terminal evidence, so the
        // seal refuses with `MissingTerminalEvidence`. An empty
        // schedule is not itself a defect — the lifecycle owner would
        // still record observation-only terminal evidence — but the
        // collector alone cannot synthesize that.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Repro/NoAction",
            quantum,
            std::time::Duration::from_secs(6),
            vec![],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        assert_eq!(program.transition_count(), 3_000);
        let mut collector = EvidenceCollector::for_program(program);
        // The empty schedule has no step outcomes to record.
        collector
            .record_capture("motion".to_owned(), CaptureRecord::State(vec![0]))
            .expect("record observation-only capture");
        let err = collector
            .seal()
            .expect_err("no-action 3000-transition run must refuse without terminal evidence");
        assert!(
            matches!(err, SealError::MissingTerminalEvidence),
            "expected MissingTerminalEvidence, got {err:?}"
        );
    }

    #[test]
    fn reproduction_n_3000_seals_from_n_minus_1_receipt_refuses_without_terminal_evidence() {
        // Probe 2 from the scenario acceptance review line 170: "N=3000 seals
        // from N-1 receipt: Ok(true)". The previous design used the
        // maximum setpoint eligibility and accepted
        // `eligibility <= transition_count - 1`. The new design
        // refuses because terminal evidence is missing: a real
        // lifecycle that observed only N - 1 receipts cannot have
        // recorded `completed_transitions = N`, and a fake one that
        // did would fail the quantum / completion / cut checks below.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Repro/NMinusOneReceipt",
            quantum,
            std::time::Duration::from_secs(6),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![],
        )
        .unwrap();
        assert_eq!(program.transition_count(), 3_000);
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record setpoint acknowledgement at boundary 0");
        let err = collector
            .seal()
            .expect_err("N=3000 with N-1 receipt must refuse without terminal evidence");
        assert!(
            matches!(err, SealError::MissingTerminalEvidence),
            "expected MissingTerminalEvidence, got {err:?}"
        );
    }

    #[test]
    fn reproduction_n_1_seals_at_boundary_0_refuses_without_terminal_evidence() {
        // Probe 3 from the scenario acceptance review line 171: "N=1 seals at
        // boundary 0: Ok(true)". A 1-transition plan with a single
        // boundary-zero setpoint acknowledgement still needs terminal
        // evidence to seal; the lifecycle alone owns that surface.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Repro/N1Boundary0",
            quantum,
            std::time::Duration::from_micros(2_000),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        assert_eq!(program.transition_count(), 1);
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record setpoint acknowledgement at boundary 0");
        collector
            .record_capture("motion".to_owned(), CaptureRecord::State(vec![1]))
            .expect("record capture");
        let err = collector
            .seal()
            .expect_err("N=1 at boundary 0 must refuse without terminal evidence");
        assert!(
            matches!(err, SealError::MissingTerminalEvidence),
            "expected MissingTerminalEvidence, got {err:?}"
        );
    }

    #[test]
    fn reproduction_command_only_3000_transition_run_with_reply_refuses_without_terminal_evidence()
    {
        // Probe 4 from the scenario acceptance review line 172: "command-only
        // 3000-transition run + reply before wrong-label issue seals:
        // Ok(true)". A command-only schedule with one command action
        // at boundary 0, an issued step outcome, and a recorded
        // reply must still refuse without terminal evidence. The
        // previous design passed this because the eligibility
        // heuristic was skipped for command-only programs; the new
        // design requires lifecycle-recorded completion.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Repro/CommandOnly3000",
            quantum,
            std::time::Duration::from_secs(6),
            vec![ScheduleEntry::at(0, command_action("do_thing", 1))],
            vec![],
        )
        .unwrap();
        assert_eq!(program.transition_count(), 3_000);
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::CommandIssued {
                    label: "do_thing".to_owned(),
                    reply_pending: true,
                    simulated_deadline_boundary: 0,
                    host_deadline_unix_micros: 0,
                },
            )
            .expect("record command");
        collector
            .record_command_reply(
                "do_thing".to_owned(),
                CommandReply::Accepted {
                    response_bytes: vec![0xff],
                },
            )
            .expect("record reply");
        let err = collector.seal().expect_err(
            "command-only 3000-transition run with reply must refuse without terminal evidence",
        );
        assert!(
            matches!(err, SealError::MissingTerminalEvidence),
            "expected MissingTerminalEvidence, got {err:?}"
        );
    }

    #[test]
    fn builder_enforces_completed_transitions_equals_program_transition_count() {
        // Required regression from the scenario acceptance review §2: the
        // builder must not derive `quantum_ns` or
        // `completed_transitions` from the program — those come
        // from the lifecycle-observed terminal facts. Without
        // explicit `with_terminal_quantum_ns` /
        // `with_completed_transitions` calls, the builder produces
        // evidence with `0` for both fields, and the seal refuses
        // any program whose quantum or transition count is
        // non-zero.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Repro/NMinusOneTerminal",
            quantum,
            std::time::Duration::from_secs(6),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        let evidence = collector
            .terminal_evidence_builder()
            .with_execution_identity("exec/scenario/builder_enforces")
            .final_observation_cut_observed()
            .final_capture_drain_observed()
            .cleanup_succeeded()
            .build();
        assert_eq!(
            evidence.completed_transitions(),
            0,
            "builder must not derive completed_transitions from the program; \
             the lifecycle must supply it via with_completed_transitions"
        );
        assert_eq!(
            evidence.quantum_ns(),
            0,
            "builder must not derive quantum_ns from the program; \
             the lifecycle must supply it via with_terminal_quantum_ns"
        );

        // Once the lifecycle calls `with_terminal_quantum_ns` and
        // `with_completed_transitions` with the actual observed
        // values, those values reach the evidence surface and the
        // seal accepts when they match the program.
        let program_quantum_ns = u64::from(collector.program().quantum().micros()) * 1_000;
        let program_transition_count = u64::from(collector.program().transition_count());
        let evidence = {
            let mut builder = collector.terminal_evidence_builder();
            builder = builder
                .with_execution_identity("exec/scenario/builder_enforces_with_facts")
                .with_terminal_quantum_ns(program_quantum_ns)
                .with_completed_transitions(program_transition_count)
                .final_observation_cut_observed()
                .final_capture_drain_observed()
                .cleanup_succeeded();
            builder.build()
        };
        assert_eq!(
            evidence.completed_transitions(),
            program_transition_count,
            "lifecycle-supplied completed_transitions must reach the evidence surface"
        );
        assert_eq!(
            evidence.quantum_ns(),
            program_quantum_ns,
            "lifecycle-supplied quantum_ns must reach the evidence surface"
        );
    }

    #[test]
    fn seal_rejects_interrupted_final_phase() {
        // Required regression from the scenario acceptance review line 195:
        // "an interrupted final phase must fail". The lifecycle
        // builder defaults the three lifecycle flags to `false`;
        // `record_terminal_evidence` refuses evidence that did not
        // observe the final cut, drain, or cleanup. Each omitted
        // method must produce the corresponding `SealError` variant.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Repro/InterruptedFinalPhase",
            quantum,
            std::time::Duration::from_secs(6),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();

        // Final observation cut omitted.
        let mut collector = EvidenceCollector::for_program(program.clone());
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
        collector
            .record_capture("motion".to_owned(), CaptureRecord::State(vec![0xAA]))
            .expect("record capture");
        let evidence = collector
            .terminal_evidence_builder()
            .with_execution_identity("exec/scenario/interrupted/cut")
            .final_capture_drain_observed()
            .cleanup_succeeded()
            .build();
        let err = collector
            .record_terminal_evidence(evidence)
            .expect_err("interrupted final phase must be refused at record time");
        assert!(
            matches!(err, SealError::MissingFinalObservationCut),
            "expected MissingFinalObservationCut, got {err:?}"
        );

        // Drain finalization omitted.
        let mut collector = EvidenceCollector::for_program(program.clone());
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
        collector
            .record_capture("motion".to_owned(), CaptureRecord::State(vec![0xAA]))
            .expect("record capture");
        let evidence = collector
            .terminal_evidence_builder()
            .with_execution_identity("exec/scenario/interrupted/drain")
            .final_observation_cut_observed()
            .cleanup_succeeded()
            .build();
        let err = collector
            .record_terminal_evidence(evidence)
            .expect_err("missing final capture drain must be refused at record time");
        assert!(
            matches!(err, SealError::MissingFinalCaptureDrain),
            "expected MissingFinalCaptureDrain, got {err:?}"
        );

        // Cleanup not succeeded.
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
        collector
            .record_capture("motion".to_owned(), CaptureRecord::State(vec![0xAA]))
            .expect("record capture");
        let evidence = collector
            .terminal_evidence_builder()
            .with_execution_identity("exec/scenario/interrupted/cleanup")
            .final_observation_cut_observed()
            .final_capture_drain_observed()
            .build();
        let err = collector
            .record_terminal_evidence(evidence)
            .expect_err("cleanup failure must be refused at record time");
        assert!(
            matches!(err, SealError::CleanupFailed),
            "expected CleanupFailed, got {err:?}"
        );
    }

    #[test]
    fn builder_enforces_quantum_ns_in_nanoseconds() {
        // Required regression from the scenario acceptance review §2: the
        // builder must not derive `quantum_ns` from the program;
        // the lifecycle supplies it via `with_terminal_quantum_ns`.
        // Absent that call, the field defaults to `0` and the
        // seal-time mismatch check rejects any program whose
        // quantum is non-zero. With the call, the value reaches
        // the evidence surface verbatim.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Repro/MismatchedQuantum",
            quantum,
            std::time::Duration::from_secs(6),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        let mut collector = EvidenceCollector::for_program(program);
        let evidence = collector
            .terminal_evidence_builder()
            .with_execution_identity("exec/scenario/mismatch_default")
            .final_observation_cut_observed()
            .final_capture_drain_observed()
            .cleanup_succeeded()
            .build();
        assert_eq!(
            evidence.quantum_ns(),
            0,
            "absent with_terminal_quantum_ns, the builder must default to 0 \
             (not derive from the program); the seal then rejects the mismatch"
        );

        // Lifecycle-supplied value reaches the evidence surface.
        let program_transition_count = u64::from(collector.program().transition_count());
        let evidence = {
            let mut builder = collector.terminal_evidence_builder();
            builder = builder
                .with_execution_identity("exec/scenario/mismatch_with_facts")
                .with_terminal_quantum_ns(2_000_000)
                .with_completed_transitions(program_transition_count)
                .final_observation_cut_observed()
                .final_capture_drain_observed()
                .cleanup_succeeded();
            builder.build()
        };
        assert_eq!(
            evidence.quantum_ns(),
            2_000_000,
            "lifecycle-supplied quantum_ns must reach the evidence surface"
        );
    }

    #[test]
    fn seal_succeeds_for_sparse_valid_run_with_real_terminal_evidence() {
        // Required regression from the scenario acceptance review line 195:
        // "a sparse valid case with real terminal evidence must
        // pass". A 6 ms / 2 ms quantum program with one setpoint at
        // boundary 0 and one state capture acts once early and
        // observes until the end. With lifecycle-recorded terminal
        // evidence that matches the program (3 completed transitions
        // at 2_000_000 ns, final observation cut, final capture
        // drain, cleanup succeeded) the seal accepts the sparse
        // schedule. This is the "sparse valid" complement to the
        // 3000-transition-with-only-boundary-zero regression above.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let program = Program::normalize(
            "scenarios/Repro/SparseValid",
            quantum,
            std::time::Duration::from_micros(6_000),
            vec![ScheduleEntry::at(0, setpoint_action(1))],
            vec![Capture::state("motion", state_sig()).expect("motion capture")],
        )
        .unwrap();
        assert_eq!(program.transition_count(), 3);
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .expect("record step");
        collector
            .record_capture("motion".to_owned(), CaptureRecord::State(vec![0xAA, 0xBB]))
            .expect("record capture");
        record_valid_terminal_evidence(&mut collector, "exec/scenario/sparse_valid");
        let run = collector.seal().expect("seal");
        assert!(run.is_sealed());
        assert!(run.passed(), "sparse valid case must pass");
    }
}
