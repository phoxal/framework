//! The standalone service-owned multistage procedure fixture.
//!
//! A service runtime — not a brain — owns one command-triggered behavior
//! tree: Busy while running, explicit stop, replacement after terminal,
//! one generated survey call per pass with a typed continuation, a settling
//! delay, and a two-pass explicit repeat. The fixture proves the behavior
//! facility is ordinary service authoring, deterministic without a
//! transport.

mod contract;

use contract::multistage_api;
use contract::{
    BeginRequest, BeginResponse, MissionState, Phase, StageEvent, StopRequest, StopResponse,
    SurveyRequest, SurveyResponse,
};
use phoxal::Result;
use phoxal::runtime::behavior::{Sequence, Tree, TreeStatus, condition, repeat, sequence};
use phoxal::runtime::{CallCompletion, Context};

struct Multistage {
    job: Option<u64>,
    mission: Option<Tree<Multistage>>,
}

#[phoxal::runtime(contract = contract::MultistageApi, period_ms = 10)]
impl Multistage {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self {
            job: None,
            mission: None,
        })
    }

    /// The reserved boundary job id: its mission's dynamic child is
    /// deliberately deeper than the aggregate depth bound, so the owner
    /// path surfaces the bounded structural validator.
    const BOUNDS_PROBE_JOB_ID: u64 = u64::MAX;

    #[handle(begin)]
    fn begin(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: BeginRequest,
    ) -> Result<BeginResponse> {
        if request.job_id == 0 {
            return Ok(BeginResponse::Invalid);
        }
        if self
            .mission
            .as_ref()
            .is_some_and(|m| m.status() == TreeStatus::Running)
        {
            return Ok(BeginResponse::Busy);
        }
        self.job = Some(request.job_id);
        self.mission = Some(if request.job_id == Self::BOUNDS_PROBE_JOB_ID {
            Self::bounds_violating_mission()?
        } else {
            Self::mission(request.job_id)?
        });
        ctx.emit_stages(StageEvent {
            job_id: request.job_id,
            stage: 0,
        })?;
        Ok(BeginResponse::Accepted)
    }

    #[handle(stop)]
    fn stop(&mut self, ctx: &mut Context<'_, Self>, request: StopRequest) -> Result<StopResponse> {
        let Some(mission) = self.mission.as_mut() else {
            return Ok(StopResponse::NotRunning);
        };
        if mission.status() != TreeStatus::Running || self.job != Some(request.job_id) {
            return Ok(StopResponse::NotRunning);
        }
        mission.cancel(ctx)?;
        if let Some(job_id) = self.job.take() {
            ctx.emit_stages(StageEvent {
                job_id,
                stage: u32::MAX,
            })?;
        }
        Ok(StopResponse::Stopped)
    }

    #[complete(survey)]
    fn survey_completed(
        &mut self,
        _ctx: &mut Context<'_, Self>,
        _completion: CallCompletion<SurveyResponse>,
    ) -> Result<()> {
        // The tree's leaves own their responses; this field handler exists
        // only because the contract declares the call, and the ledger never
        // routes a tree-staged ticket here.
        Ok(())
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> Result<()> {
        if let Some(mission) = self.mission.as_mut() {
            let before = mission.status();
            mission.tick(ctx)?;
            if before == TreeStatus::Running
                && mission.status() != TreeStatus::Running
                && let Some(job_id) = self.job
            {
                ctx.emit_stages(StageEvent { job_id, stage: 99 })?;
            }
        }
        Ok(())
    }

    #[publish(status)]
    fn status(&self) -> MissionState {
        MissionState {
            job_id: self.job,
            phase: self.phase(),
        }
    }

    fn phase(&self) -> Phase {
        match self.mission.as_ref().map(|m| m.status()) {
            Some(TreeStatus::Running) => Phase::Surveying,
            Some(TreeStatus::Succeeded) => Phase::Succeeded,
            Some(TreeStatus::Cancelled) => Phase::Cancelled,
            _ => Phase::Idle,
        }
    }

    /// One deliberately over-deep dynamic child: the bounded structural
    /// validator refuses it before activation, faulting the invocation
    /// that would otherwise execute it.
    fn bounds_violating_mission() -> Result<Tree<Multistage>> {
        repeat::<Self, _>(1, |_| {
            let mut node = condition(|_| true);
            for _ in 0..phoxal::runtime::behavior::MAX_DEPTH + 1 {
                node = phoxal::runtime::behavior::guard(|_| true, node);
            }
            Ok(node)
        })
        .build()
    }

    /// The multistage mission: a surveyed pass consumed by one typed
    /// continuation, a settling delay, then one explicit repeated pass.
    fn mission(job_id: u64) -> Result<Tree<Multistage>> {
        sequence([
            Sequence::<Self>::new()
                .call(multistage_api::calls::survey(SurveyRequest { pass: 0 }))
                .then(move |response: SurveyResponse| {
                    let observed = response.reading;
                    Ok(condition(move |_| observed == job_id))
                })
                .delay(std::time::Duration::from_millis(20))
                .into_node(),
            repeat::<Self, _>(1, |attempt| {
                Ok(Sequence::<Self>::new()
                    .call(multistage_api::calls::survey(SurveyRequest {
                        pass: u64::from(attempt) + 1,
                    }))
                    .expect_response(|_reply: &SurveyResponse| true)
                    .into_node())
            }),
        ])
        .build()
    }
}

fn main() -> Result<()> {
    phoxal::runtime::run::<Multistage>()
}

/// Deterministic owner-path proof: a service-owned tree advances through
/// its stages, refuses stale readings, cancels on command, and accepts a
/// replacement after every terminal outcome. The adapter stays readable so
/// each staged ticket is composed from the epoch initialization actually
/// drew; a fresh adapter per test keeps parallel tests independent.
#[cfg(test)]
mod tests {
    use super::contract::multistage_api;
    use super::contract::{BeginRequest, BeginResponse, StopRequest, StopResponse, SurveyResponse};
    use phoxal::contracts::ProstPayload;
    use phoxal::runtime::input::{
        CommandOrder, Commands, InputSnapshot, TransportCallCompletion, TransportInputSink,
    };
    use phoxal::runtime::outputs::compose_call_ticket;
    use phoxal::runtime::{
        CommandId, ExecutionDuration, ExecutionTime, StepContext, initialize, invoke,
    };

    struct Fixture {
        epoch: u64,
        adapter: super::phoxal_runtime_multistage::Adapter,
        service: Option<super::Multistage>,
    }

    impl Fixture {
        fn new() -> Self {
            let adapter = super::phoxal_runtime_multistage::Adapter::new();
            let service =
                initialize(&adapter, ExecutionTime::default(), ()).expect("fixture initializes");
            Self {
                epoch: adapter.execution_epoch(),
                adapter,
                service: Some(service),
            }
        }

        fn step(
            &mut self,
            context: &StepContext,
            inputs: &multistage_api::Inputs,
        ) -> phoxal::Result<multistage_api::Outputs> {
            let service = self
                .service
                .take()
                .expect("the service exists between steps");
            let (service, outputs) = invoke(&self.adapter, context, service, inputs)?;
            self.service = Some(service);
            Ok(outputs)
        }
    }

    fn context(index: u64, millis: u64) -> StepContext {
        StepContext::from_previous(
            ExecutionTime::from_nanos(millis * 1_000_000),
            ExecutionDuration::from_millis(10),
            Some(ExecutionTime::from_nanos(
                millis.saturating_sub(10) * 1_000_000,
            )),
            0,
            index,
        )
    }

    fn begin(job_id: u64) -> multistage_api::Inputs {
        let mut inputs = <multistage_api::Inputs as InputSnapshot>::empty();
        inputs.begin = Commands::new(vec![phoxal::runtime::Command::with_order(
            CommandOrder::new(1, 0, CommandId::new(1)),
            BeginRequest { job_id },
        )]);
        inputs
    }

    fn stop(job_id: u64) -> multistage_api::Inputs {
        let mut inputs = <multistage_api::Inputs as InputSnapshot>::empty();
        inputs.stop = Commands::new(vec![phoxal::runtime::Command::with_order(
            CommandOrder::new(1, 0, CommandId::new(2)),
            StopRequest { job_id },
        )]);
        inputs
    }

    fn completion(
        fixture: &Fixture,
        invocation: u64,
        reading: u64,
    ) -> phoxal::Result<multistage_api::Inputs> {
        let ticket = compose_call_ticket(fixture.epoch, invocation, 0)
            .expect("the fixture's ticket space is representable");
        let mut inputs = <multistage_api::Inputs as InputSnapshot>::empty();
        inputs.set_call_completions(vec![TransportCallCompletion {
            ticket,
            result: Ok(SurveyResponse { reading }.encode_payload()?),
        }])?;
        Ok(inputs)
    }

    fn empty() -> multistage_api::Inputs {
        <multistage_api::Inputs as InputSnapshot>::empty()
    }

    #[test]
    fn the_multistage_mission_completes_through_every_stage() -> phoxal::Result<()> {
        let mut fixture = Fixture::new();
        let accepted = fixture.step(&context(0, 0), &begin(7))?;
        assert!(matches!(
            accepted.begin_replies[0].response(),
            BeginResponse::Accepted
        ));
        assert_eq!(accepted.stages.len(), 1);

        // Busy while the mission runs.
        let busy = fixture.step(&context(1, 10), &begin(8))?;
        assert!(matches!(
            busy.begin_replies[0].response(),
            BeginResponse::Busy
        ));

        // Pass 0's survey reply arrives at the next invocation; its typed
        // continuation accepts the reading and the settle delay follows.
        let surveyed = fixture.step(&context(2, 20), &completion(&fixture, 0, 7)?)?;
        assert_eq!(surveyed.stages.len(), 0);

        // The delay completes at 20 ms after its anchor.
        fixture.step(&context(3, 40), &empty())?;

        // The repeated pass stages its own survey at invocation 3; its
        // reply at invocation 4 completes the mission.
        let repeated = fixture.step(&context(4, 50), &completion(&fixture, 3, 7)?)?;
        assert_eq!(
            repeated.stages.last().map(|stage| stage.stage),
            Some(99),
            "the terminal stage event fired"
        );

        // A terminal tree stays terminal: later ticks add no events.
        let quiet = fixture.step(&context(5, 60), &empty())?;
        assert!(
            quiet.stages.is_empty(),
            "a terminal mission replays nothing"
        );

        // A terminal mission is replaceable.
        let replaced = fixture.step(&context(6, 70), &begin(9))?;
        assert!(matches!(
            replaced.begin_replies[0].response(),
            BeginResponse::Accepted
        ));
        assert_eq!(
            replaced.stages.last().map(|stage| stage.job_id),
            Some(9),
            "the replacement carries its own domain identity"
        );
        Ok(())
    }

    /// The bounds-probe job's dynamic child is refused before
    /// activation: the invocation faults with the bounded validator's
    /// error instead of executing the over-deep subtree.
    #[test]
    fn a_dynamic_child_beyond_the_depth_bound_faults_the_invocation() -> phoxal::Result<()> {
        let mut fixture = Fixture::new();
        // The bounds-probe mission ticks in the same invocation as its
        // begin: the over-deep dynamic child is refused before activation
        // and the invocation faults with the validator's error.
        let fault = fixture.step(&context(0, 0), &begin(u64::MAX));
        let error = fault
            .err()
            .expect("the over-deep child faults its invocation");
        assert!(
            error.to_string().contains("depth"),
            "the fault names the depth bound: {error}"
        );
        Ok(())
    }

    #[test]
    fn a_stale_reading_is_refused_and_stop_cancels() -> phoxal::Result<()> {
        let mut fixture = Fixture::new();
        fixture.step(&context(0, 0), &begin(7))?;

        // A reading for a different job fails the continuation's condition:
        // the mission ends Refused rather than retrying, with its terminal
        // evidence retained.
        let refused = fixture.step(&context(1, 10), &completion(&fixture, 0, 8)?)?;
        assert!(
            matches!(refused.stages.last().map(|stage| stage.stage), Some(99)),
            "the refused mission retained its terminal evidence"
        );

        // A refused mission is terminal and replaceable.
        let replacement = fixture.step(&context(2, 20), &begin(9))?;
        assert!(
            matches!(
                replacement.begin_replies[0].response(),
                BeginResponse::Accepted
            ),
            "a refused mission is terminal and replaceable"
        );

        // A running mission stops on command and retains its terminal
        // evidence; a stop of a terminal mission is NotRunning; a cancelled
        // mission is replaceable.
        let stopped = fixture.step(&context(3, 30), &stop(9))?;
        assert!(matches!(
            stopped.stop_replies[0].response(),
            StopResponse::Stopped
        ));
        assert_eq!(
            stopped.stages.last().map(|stage| stage.stage),
            Some(u32::MAX),
            "the cancelled mission retained its terminal evidence"
        );
        let again = fixture.step(&context(4, 40), &stop(9))?;
        assert!(matches!(
            again.stop_replies[0].response(),
            StopResponse::NotRunning
        ));
        let resumed = fixture.step(&context(5, 50), &begin(6))?;
        assert!(
            matches!(resumed.begin_replies[0].response(), BeginResponse::Accepted),
            "a cancelled mission is terminal and replaceable"
        );
        Ok(())
    }
}
