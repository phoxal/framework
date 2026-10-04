//! The standalone service-owned multistage procedure fixture.
//!
//! A service runtime — not a brain — owns one command-triggered behavior
//! tree: Busy while running, explicit stop, replacement after terminal,
//! one generated survey call per pass with a typed continuation, a settling
//! delay, and a two-pass explicit repeat. The fixture proves the behavior
//! facility is ordinary service authoring, deterministic without a
//! transport.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

mod contract;

use contract::MultistageApi;
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
                .call(MultistageApi::survey(SurveyRequest { pass: 0 }))
                .then(move |response: SurveyResponse| {
                    let observed = response.reading;
                    Ok(condition(move |_| observed == job_id))
                })
                .delay(std::time::Duration::from_millis(20))
                .into_node(),
            repeat::<Self, _>(1, |attempt| {
                Ok(Sequence::<Self>::new()
                    .call(MultistageApi::survey(SurveyRequest {
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

/// The public harness drives the declared cadence and real owner admission.
#[cfg(test)]
mod tests {
    use super::{Multistage, contract::*};
    use phoxal::runtime::Harness;
    use std::time::Duration;

    fn survey(harness: &mut Harness<Multistage>, reading: u64) -> phoxal::Result<()> {
        let request = harness
            .take_request::<SurveyRequest, SurveyResponse>("survey")?
            .expect("the mission accepted a survey request");
        harness.complete_request(&request, Ok(SurveyResponse { reading }))
    }

    #[test]
    fn the_multistage_mission_completes_through_every_stage() -> phoxal::Result<()> {
        let mut harness = Harness::<Multistage>::new(())?;
        let begin = harness.enqueue_begin(BeginRequest { job_id: 7 })?;
        harness.advance_to(Duration::ZERO)?;
        assert!(matches!(harness.reply(begin)?, BeginResponse::Accepted));
        assert_eq!(harness.stages().len(), 1);
        let busy = harness.enqueue_begin(BeginRequest { job_id: 8 })?;
        harness.advance_to(Duration::from_millis(10))?;
        assert!(matches!(harness.reply(busy)?, BeginResponse::Busy));
        survey(&mut harness, 7)?;
        harness.advance_to(Duration::from_millis(20))?;
        assert!(harness.stages().is_empty());
        harness.advance_to(Duration::from_millis(40))?;
        survey(&mut harness, 7)?;
        harness.advance_to(Duration::from_millis(50))?;
        assert_eq!(harness.stages().last().map(|stage| stage.stage), Some(99));
        harness.advance_to(Duration::from_millis(60))?;
        assert!(
            harness.stages().is_empty(),
            "a terminal mission replays nothing"
        );
        let replaced = harness.enqueue_begin(BeginRequest { job_id: 9 })?;
        harness.advance_to(Duration::from_millis(70))?;
        assert!(matches!(harness.reply(replaced)?, BeginResponse::Accepted));
        assert_eq!(harness.stages().last().map(|stage| stage.job_id), Some(9));
        Ok(())
    }

    #[test]
    fn a_dynamic_child_beyond_the_depth_bound_faults_the_invocation() -> phoxal::Result<()> {
        let mut harness = Harness::<Multistage>::new(())?;
        harness.enqueue_begin(BeginRequest { job_id: u64::MAX })?;
        let error = harness
            .advance_to(Duration::ZERO)
            .expect_err("the over-deep child faults");
        assert!(error.to_string().contains("depth"), "{error}");
        Ok(())
    }

    #[test]
    fn a_stale_reading_is_refused_and_stop_cancels() -> phoxal::Result<()> {
        let mut harness = Harness::<Multistage>::new(())?;
        let begin = harness.enqueue_begin(BeginRequest { job_id: 7 })?;
        harness.advance_to(Duration::ZERO)?;
        assert!(matches!(harness.reply(begin)?, BeginResponse::Accepted));
        harness.stages();
        survey(&mut harness, 8)?;
        harness.advance_to(Duration::from_millis(10))?;
        assert_eq!(
            harness.stages().last().map(|stage| stage.stage),
            Some(99),
            "refusal retains terminal evidence"
        );
        let replacement = harness.enqueue_begin(BeginRequest { job_id: 9 })?;
        harness.advance_to(Duration::from_millis(20))?;
        assert!(matches!(
            harness.reply(replacement)?,
            BeginResponse::Accepted
        ));
        harness.stages();
        let stop = harness.enqueue_stop(StopRequest { job_id: 9 })?;
        harness.advance_to(Duration::from_millis(30))?;
        assert!(matches!(harness.reply(stop)?, StopResponse::Stopped));
        assert_eq!(
            harness.stages().last().map(|stage| stage.stage),
            Some(u32::MAX)
        );
        let again = harness.enqueue_stop(StopRequest { job_id: 9 })?;
        harness.advance_to(Duration::from_millis(40))?;
        assert!(matches!(harness.reply(again)?, StopResponse::NotRunning));
        let resumed = harness.enqueue_begin(BeginRequest { job_id: 6 })?;
        harness.advance_to(Duration::from_millis(50))?;
        assert!(matches!(harness.reply(resumed)?, BeginResponse::Accepted));
        Ok(())
    }
}
