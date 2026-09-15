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

/// One completed simulated experiment. P3 replacement: the typed run
/// record assembled by the case host after the simulator drives the
/// fixture to completion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScenarioRun {
    /// Per-step outcomes, indexed by the step label declared in the
    /// scenario plan. Preserves the original order.
    pub step_outcomes: Vec<(String, StepOutcome)>,
    /// Per-capture observations, indexed by capture name. Native
    /// samples are stored as opaque `Vec<u8>` to avoid coupling the
    /// scenario SDK to the runtime's serializer.
    pub captures: BTreeMap<String, CaptureRecord>,
    /// Per-command acknowledgement records, indexed by the correlation
    /// label. `Accepted` records carry the application's reply bytes;
    /// `Expired` records explain why the host dropped the command.
    pub command_replies: BTreeMap<String, CommandReply>,
    /// Whether the host considers the run successful. The verify
    /// callback may reject on top of this.
    pub passed: bool,
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

impl ScenarioRun {
    /// Assemble a `ScenarioRun` from the participant's fixture
    /// trace, the captured observations, and the per-command
    /// reply records. `passed` is true iff every step outcome is
    /// not `Rejected` and every command was acknowledged.
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
        }
    }

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::participant::FixtureParticipant;
    use crate::scenario::plan::Action;
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
}
