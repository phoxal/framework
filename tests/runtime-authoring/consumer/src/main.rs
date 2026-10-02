//! The authored brain consumer for the countdown acceptance graph.
//!
//! The brain occupies the bundle's brain slot and receives the countdown
//! provider's finished events through the robot graph's
//! `brain.countdown_finished: countdown.finished` connection; its retained
//! state exposes exactly the events its handler has handled.

phoxal::api!();

mod contract;
mod provider;

use crate::api::operations::phoxal::tests::authoring::countdown::v1::Start;
use crate::api::types::phoxal::tests::authoring::countdown::v1::{StartRequest, StartResponse};
use crate::contract::ConsumedEventState;
use crate::provider::{FinishedEvent, Outcome};
use phoxal::Result;
use phoxal::runtime::Context;

struct Brain {
    mission: phoxal::runtime::behavior::Tree<Brain>,
    last_job_id: Option<u64>,
    last_outcome: Option<Outcome>,
    handled_count: u64,
    direct_replies: u64,
}

impl Brain {
    /// The overview's sequence against the mission's own dedicated
    /// countdown instance: wait for idle evidence, start one job, require
    /// its accepted response, wait for completion, then hold a bounded
    /// window before declaring success. The dedicated instance makes the
    /// sequence deterministic — no startup grace or retry policy is
    /// coupled to other traffic on the shared provider.
    fn fresh_mission() -> Result<phoxal::runtime::behavior::Tree<Brain>> {
        let mission_job = 42;
        phoxal::runtime::behavior::Sequence::<Self>::new()
            .wait_until(|ctx| {
                ctx.countdown_status()
                    .fresh()
                    .is_some_and(|status| status.active_job_id.is_none())
            })
            .call(crate::contract::brain_api::calls::start_countdown(
                StartRequest {
                    job_id: mission_job,
                    duration_ms: 500,
                },
            ))
            .expect_response(|response: &StartResponse| matches!(response, StartResponse::Accepted))
            .wait_until(move |ctx| {
                ctx.countdown_status().fresh().is_some_and(|status| {
                    status.last_job_id == Some(mission_job)
                        && matches!(status.last_outcome, Outcome::Completed)
                })
            })
            .delay(std::time::Duration::from_millis(100))
            .within(std::time::Duration::from_secs(10))
            .build()
    }
}

#[phoxal::runtime(contract = contract::BrainApi, period_ms = 20)]
impl Brain {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self {
            mission: Self::fresh_mission()?,
            last_job_id: None,
            last_outcome: None,
            handled_count: 0,
            direct_replies: 0,
        })
    }

    #[handle(countdown_finished)]
    fn on_finished(&mut self, _ctx: &mut Context<'_, Self>, event: FinishedEvent) -> Result<()> {
        self.last_job_id = Some(event.job_id);
        self.last_outcome = Some(event.outcome);
        self.handled_count = self.handled_count.saturating_add(1);
        Ok(())
    }

    #[complete(start_countdown)]
    fn start_completed(
        &mut self,
        _ctx: &mut Context<'_, Self>,
        completion: phoxal::runtime::input::CallCompletion<StartResponse>,
    ) -> Result<()> {
        // Only tickets the tree does not own arrive here: concurrent
        // direct calls to the same endpoint, each exactly once.
        self.direct_replies = self.direct_replies.saturating_add(1);
        let _ = completion.ticket();
        Ok(())
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> Result<()> {
        self.mission.tick(ctx)
    }

    #[publish(handled)]
    fn handled(&self) -> ConsumedEventState {
        ConsumedEventState {
            last_job_id: self.last_job_id,
            last_outcome: self.last_outcome,
            handled_count: self.handled_count,
        }
    }

    #[publish(command)]
    fn command(&self) -> Option<phoxal::contracts::component::actuator::ActuatorSetpoint> {
        // The leased actuator projection: one zero-velocity command for the
        // simulated mission motor, re-derived from the runtime state.
        Some(phoxal::contracts::component::actuator::ActuatorSetpoint {
            targets: vec![phoxal::contracts::component::actuator::ActuatorTarget {
                actuator_id: "mission_motor".to_owned(),
                control: Some(phoxal::contracts::component::actuator::Control::VelocityRadps(0.0)),
            }],
        })
    }

    #[publish(mission)]
    fn mission(&self) -> contract::MissionState {
        use phoxal::runtime::behavior::TreeStatus as Status;
        let phase = match self.mission.status() {
            Status::Running => contract::MissionPhase::Running,
            Status::Succeeded => contract::MissionPhase::Succeeded,
            Status::Refused => contract::MissionPhase::Refused,
            Status::Failed => contract::MissionPhase::Failed,
            Status::Cancelled => contract::MissionPhase::Cancelled,
            Status::TimedOut => contract::MissionPhase::TimedOut,
        };
        contract::MissionState { phase }
    }
}

fn main() -> Result<()> {
    phoxal::runtime::run::<Brain>()
}

#[cfg(test)]
mod expected_record;

#[cfg(test)]
mod tests {
    use super::Brain;
    use super::contract::brain_api;
    use super::expected_record::expected_runtime_record;
    use super::phoxal_runtime_brain;
    use super::provider::{FinishedEvent, Outcome};
    use phoxal::artifact::RuntimeRecord;
    use phoxal::runtime::input::{InputSet, InputSnapshot, Samples};
    use phoxal::runtime::outputs::OutputBindings;
    use phoxal::runtime::{
        ExecutionDuration, ExecutionTime, LaunchedRuntime, ObservationStamp, RegisteredRuntime,
        Sample, StepContext,
    };

    /// Compares the fixture's shared expected record against the complete
    /// record the macro retained in this binary, keeping the process
    /// bundle's manifest consistent with the binary.
    #[test]
    fn the_retained_artifact_record_matches_the_shared_expected_record() {
        let record = phoxal_runtime_brain::ARTIFACT.as_bytes();
        let payload = &record[12..];
        let decoded: RuntimeRecord = serde_json::from_slice(payload)
            .expect("the retained artifact record decodes as a runtime record");
        assert_eq!(
            decoded,
            expected_runtime_record(),
            "the compiled binary's retained record must match the shared expected record"
        );
    }

    #[test]
    fn the_authored_brain_launches_and_counts_handled_events() {
        // The type-only launch spelling binds for the consumer as well.
        let launch: fn() -> phoxal::Result<()> = <Brain as LaunchedRuntime>::launch;
        let _ = launch;
        let input_field = <brain_api::Inputs as InputSet>::FIELDS
            .iter()
            .find(|field| field.name == "countdown_finished")
            .expect("the queue input field exists");
        assert_eq!(input_field.max_items, Some(16));
        assert_eq!(input_field.max_bytes, Some(4_096));
        for field in <phoxal_runtime_brain::Adapter as OutputBindings>::FIELDS {
            if field.name == "handled" {
                assert!(
                    field.bootstrap,
                    "the consumed-event state publishes initially"
                );
            }
        }
        phoxal_runtime_brain::Adapter::retain_artifact_metadata();

        // One admitted finished event reaches the handler exactly once and
        // the resulting authored state records job identity, outcome, and
        // count.
        let adapter = phoxal_runtime_brain::Adapter::new();
        let brain = phoxal::runtime::initialize(&adapter, ExecutionTime::default(), ())
            .expect("the brain initializes");
        let mut inputs = <brain_api::Inputs as InputSnapshot>::empty();
        inputs.countdown_finished = Samples::new(vec![Sample::new(
            FinishedEvent {
                job_id: 4,
                outcome: Outcome::Completed,
            },
            ObservationStamp::new("countdown", ExecutionTime::default(), None),
        )]);
        let context =
            StepContext::first(ExecutionTime::default(), ExecutionDuration::from_millis(20));
        let (brain, _outputs) =
            phoxal::runtime::invoke(&adapter, &context, brain, &inputs).expect("the brain steps");
        assert_eq!(brain.last_job_id, Some(4));
        assert!(matches!(brain.last_outcome, Some(Outcome::Completed)));
        assert_eq!(brain.handled_count, 1);
    }
}
