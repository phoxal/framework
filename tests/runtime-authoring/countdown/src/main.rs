//! The standalone authored countdown provider.
//!
//! The binary owns its contract and runtime privately and launches through
//! the accepted type-only spelling; configuration comes only from the
//! immutable bundle named by the supervisor's launch contract.

mod contract;

use crate::contract::{
    CancelRequest, CancelResponse, CountdownState, FinishedEvent, Outcome, StartRequest,
    StartResponse,
};
use phoxal::Result;
use phoxal::runtime::Context;

struct Job {
    id: u64,
    deadline_ns: u64,
}

struct Countdown {
    active: Option<Job>,
    last: Option<(u64, Outcome)>,
}

#[phoxal::runtime(contract = contract::CountdownApi, period_ms = 20)]
impl Countdown {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self {
            active: None,
            last: None,
        })
    }

    #[handle(start)]
    fn start(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: StartRequest,
    ) -> Result<StartResponse> {
        if request.job_id == 0 || !(1..=60_000).contains(&request.duration_ms) {
            return Ok(StartResponse::Invalid);
        }
        if self.active.is_some() {
            return Ok(StartResponse::Busy);
        }
        if self
            .last
            .as_ref()
            .is_some_and(|(id, _)| *id == request.job_id)
        {
            return Ok(StartResponse::Invalid);
        }
        let Some(deadline_ns) = ctx
            .now()
            .as_nanos()
            .checked_add(request.duration_ms * 1_000_000)
        else {
            return Ok(StartResponse::Invalid);
        };
        self.active = Some(Job {
            id: request.job_id,
            deadline_ns,
        });
        Ok(StartResponse::Accepted)
    }

    #[handle(cancel)]
    fn cancel(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: CancelRequest,
    ) -> Result<CancelResponse> {
        if self
            .active
            .as_ref()
            .is_some_and(|job| job.id == request.job_id)
        {
            self.finish(ctx, Outcome::Cancelled)?;
            Ok(CancelResponse::Cancelled)
        } else {
            Ok(CancelResponse::UnknownJob)
        }
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> Result<()> {
        if self
            .active
            .as_ref()
            .is_some_and(|job| ctx.now().as_nanos() >= job.deadline_ns)
        {
            self.finish(ctx, Outcome::Completed)?;
        }
        Ok(())
    }

    #[publish(status)]
    fn status(&self) -> CountdownState {
        CountdownState {
            active_job_id: self.active.as_ref().map(|job| job.id),
            last_job_id: self.last.as_ref().map(|(id, _)| *id),
            last_outcome: self
                .last
                .as_ref()
                .map_or(Outcome::Unspecified, |(_, outcome)| *outcome),
        }
    }

    fn finish(&mut self, ctx: &mut Context<'_, Self>, outcome: Outcome) -> Result<()> {
        if let Some(job) = self.active.take() {
            ctx.emit_finished(FinishedEvent {
                job_id: job.id,
                outcome,
            })?;
            self.last = Some((job.id, outcome));
        }
        Ok(())
    }
}

fn main() -> Result<()> {
    phoxal::runtime::run::<Countdown>()
}

#[cfg(test)]
mod expected_record;

#[cfg(test)]
mod tests {
    use super::Countdown;
    use super::contract::countdown_api;
    use super::contract::{CancelRequest, Outcome, StartRequest, StartResponse};
    use super::expected_record::expected_runtime_record;
    use super::phoxal_runtime_countdown;
    use phoxal::artifact::RuntimeRecord;
    use phoxal::runtime::input::InputSet;
    use phoxal::runtime::outputs::{OutputBindings, OutputSet};
    use phoxal::runtime::{
        ExecutionDuration, ExecutionTime, LaunchedRuntime, RegisteredRuntime, StepContext,
    };

    /// Compares the fixture's shared expected record against the complete
    /// record the macro retained in this binary's linked artifact section.
    /// Supervisor admission does not perform this comparison against the
    /// manifest; this test is what makes the shared fixture a real
    /// consistency guarantee for the process bundle.
    #[test]
    fn the_retained_artifact_record_matches_the_shared_expected_record() {
        let record = phoxal_runtime_countdown::ARTIFACT.as_bytes();
        // The native record is `[8-byte magic][4-byte length][JSON]\n`.
        let payload = &record[12..];
        let decoded: RuntimeRecord = serde_json::from_slice(payload)
            .expect("the retained artifact record decodes as a runtime record");
        assert_eq!(
            decoded,
            expected_runtime_record(),
            "the compiled binary's retained record must match the shared \
             expected record used by the process bundle"
        );
    }

    /// Drives one owner invocation at an explicit execution time.
    fn accept_at(
        owner: &mut phoxal::runtime::RuntimeOwner<phoxal_runtime_countdown::Adapter>,
        millis: u64,
        index: u64,
        inputs: &countdown_api::Inputs,
    ) -> phoxal::Result<countdown_api::Outputs> {
        let period = ExecutionDuration::from_millis(20);
        let context = if index == 0 {
            StepContext::first(ExecutionTime::from_nanos(millis * 1_000_000), period)
        } else {
            StepContext::from_previous(
                ExecutionTime::from_nanos(millis * 1_000_000),
                period,
                Some(ExecutionTime::from_nanos(
                    millis.saturating_sub(20) * 1_000_000,
                )),
                0,
                index,
            )
        };
        Ok(owner.accept(&context, inputs)?.into_outputs())
    }

    fn start_inputs(job_id: u64, duration_ms: u64) -> countdown_api::Inputs {
        use phoxal::runtime::input::{Command, CommandId, CommandOrder, Commands};

        let mut inputs = <countdown_api::Inputs as phoxal::runtime::input::InputSnapshot>::empty();
        inputs.start = Commands::new(vec![Command::with_order(
            CommandOrder::new(1, 0, CommandId::new(1)),
            StartRequest {
                job_id,
                duration_ms,
            },
        )]);
        inputs
    }

    fn cancel_inputs(job_id: u64) -> countdown_api::Inputs {
        use phoxal::runtime::input::{Command, CommandId, CommandOrder, Commands};

        let mut inputs = <countdown_api::Inputs as phoxal::runtime::input::InputSnapshot>::empty();
        inputs.cancel = Commands::new(vec![Command::with_order(
            CommandOrder::new(1, 0, CommandId::new(1)),
            CancelRequest { job_id },
        )]);
        inputs
    }

    fn empty_inputs() -> countdown_api::Inputs {
        <countdown_api::Inputs as phoxal::runtime::input::InputSnapshot>::empty()
    }

    /// The overview's explicit-time harness test, running against this
    /// binary's actual countdown implementation.
    #[test]
    fn the_overview_execution_time_test_runs_on_the_real_implementation() -> phoxal::Result<()> {
        use std::time::Duration;

        use phoxal::runtime::Harness;

        let mut host = Harness::<Countdown>::new(())?;
        let call = host.enqueue_start(StartRequest {
            job_id: 1,
            duration_ms: 3_000,
        })?;

        host.advance_to(Duration::ZERO)?;
        assert!(matches!(host.reply(call)?, StartResponse::Accepted));

        host.advance_to(Duration::from_millis(2_999))?;
        assert!(host.finished().is_empty());

        host.advance_to(Duration::from_secs(3))?;
        let finished = host.finished();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].job_id, 1);
        assert!(matches!(finished[0].outcome, Outcome::Completed));
        Ok(())
    }

    /// Harness reset discards staged inputs and retained effects, restarts
    /// the local release schedule, and captures only the fresh bootstrap
    /// publication. In-memory only: no transport timeline fence.
    #[test]
    fn harness_reset_discards_old_work_and_republishes_initial_state() -> phoxal::Result<()> {
        use std::time::Duration;

        use phoxal::runtime::Harness;

        let mut host = Harness::<Countdown>::new(())?;
        // One accepted completion exists, then a second (staged, unexecuted)
        // request is pending when the reset happens.
        let call = host.enqueue_start(StartRequest {
            job_id: 5,
            duration_ms: 40,
        })?;
        host.advance_to(Duration::ZERO)?;
        assert!(matches!(host.reply(call)?, StartResponse::Accepted));
        host.advance_to(Duration::from_millis(60))?;
        assert_eq!(host.finished().len(), 1);
        let stale = host.enqueue_start(StartRequest {
            job_id: 2,
            duration_ms: 100,
        })?;

        host.reset(())?;
        // The retained effect and the staged request are gone; the fresh
        // bootstrap publication is the initial state again.
        assert!(host.finished().is_empty());
        let status = host.status().expect("bootstrap state is retained");
        assert_eq!(status.active_job_id, None);
        assert_eq!(status.last_job_id, None);
        assert_eq!(status.last_outcome, Outcome::Unspecified);
        // The stale staged call's candidate was discarded with the reset:
        // its reply will never arrive.
        assert!(matches!(
            host.reply(stale),
            Err(phoxal::runtime::HarnessError::ReplyConsumed)
        ));
        // The fresh schedule restarts at the reset instant: advancing to it
        // executes the first fresh release, and the retired fence's job
        // identifier is acceptable again on a fresh domain decision.
        host.advance_to(Duration::from_millis(60))?;
        let recall = host.enqueue_start(StartRequest {
            job_id: 5,
            duration_ms: 40,
        })?;
        host.advance_to(Duration::from_millis(80))?;
        assert!(matches!(host.reply(recall)?, StartResponse::Accepted));
        Ok(())
    }

    #[test]
    fn the_deadline_fires_exactly_once_at_the_execution_time_boundary() -> phoxal::Result<()> {
        use phoxal::runtime::RuntimeOwner;

        let mut owner = RuntimeOwner::new(
            phoxal_runtime_countdown::Adapter::new(),
            ExecutionTime::default(),
            (),
        )?;
        let outputs = accept_at(&mut owner, 0, 0, &start_inputs(1, 3_000))?;
        assert!(matches!(
            outputs.start_replies[0].response(),
            StartResponse::Accepted
        ));

        // Every invocation strictly before the 3000 ms deadline produces no
        // finished event.
        for (index, millis) in (20..=2_980).step_by(20).enumerate() {
            let outputs = accept_at(&mut owner, millis, (index + 1) as u64, &empty_inputs())?;
            assert!(
                outputs.finished.is_empty(),
                "no finished event before the deadline at {millis} ms"
            );
        }

        // Exactly at the deadline the event fires once.
        let outputs = accept_at(&mut owner, 3_000, 150, &empty_inputs())?;
        assert_eq!(outputs.finished.len(), 1);
        assert_eq!(outputs.finished[0].job_id, 1);
        assert!(matches!(outputs.finished[0].outcome, Outcome::Completed));

        // Later invocations never duplicate the terminal event.
        for (index, millis) in (3_020..=3_080).step_by(20).enumerate() {
            let outputs = accept_at(&mut owner, millis, (151 + index) as u64, &empty_inputs())?;
            assert!(
                outputs.finished.is_empty(),
                "no duplicate event at {millis} ms"
            );
        }
        Ok(())
    }

    #[test]
    fn cancellation_before_the_deadline_never_completes_the_job() -> phoxal::Result<()> {
        use phoxal::runtime::RuntimeOwner;

        let mut owner = RuntimeOwner::new(
            phoxal_runtime_countdown::Adapter::new(),
            ExecutionTime::default(),
            (),
        )?;
        let outputs = accept_at(&mut owner, 0, 0, &start_inputs(5, 5_000))?;
        assert!(matches!(
            outputs.start_replies[0].response(),
            StartResponse::Accepted
        ));

        let outputs = accept_at(&mut owner, 1_000, 1, &cancel_inputs(5))?;
        assert_eq!(outputs.finished.len(), 1);
        assert_eq!(outputs.finished[0].job_id, 5);
        assert!(matches!(outputs.finished[0].outcome, Outcome::Cancelled));

        // Past the original deadline the cancelled job never completes.
        for (index, millis) in (5_000..=5_100).step_by(20).enumerate() {
            let outputs = accept_at(&mut owner, millis, (2 + index) as u64, &empty_inputs())?;
            assert!(outputs.finished.is_empty());
        }
        Ok(())
    }

    #[test]
    fn invalid_durations_and_deadline_overflow_are_refused_without_events() -> phoxal::Result<()> {
        use phoxal::runtime::RuntimeOwner;

        let mut owner = RuntimeOwner::new(
            phoxal_runtime_countdown::Adapter::new(),
            ExecutionTime::default(),
            (),
        )?;
        for (attempt, duration) in [0_u64, 60_001_u64].iter().enumerate() {
            let outputs = accept_at(
                &mut owner,
                (attempt as u64) * 20,
                attempt as u64,
                &start_inputs(9, *duration),
            )?;
            assert!(
                matches!(outputs.start_replies[0].response(), StartResponse::Invalid),
                "duration {duration} ms is invalid"
            );
            assert!(outputs.finished.is_empty());
        }

        // A deadline computation that overflows the execution timeline is
        // refused as a domain response, never a panic.
        let mut owner = RuntimeOwner::new(
            phoxal_runtime_countdown::Adapter::new(),
            ExecutionTime::default(),
            (),
        )?;
        let far_future = ExecutionTime::from_nanos(u64::MAX - 1_000_000_000);
        let context = StepContext::first(far_future, ExecutionDuration::from_millis(20));
        let inputs = start_inputs(11, 60_000);
        let outputs = owner.accept(&context, &inputs)?.into_outputs();
        assert!(
            matches!(outputs.start_replies[0].response(), StartResponse::Invalid),
            "a deadline past the end of the execution timeline is invalid"
        );
        assert!(outputs.finished.is_empty());
        Ok(())
    }

    #[test]
    fn the_authored_runtime_launches_and_records_its_contract() {
        // The type-only launch spelling binds for a privately owned binary
        // runtime without constructing a value or naming the adapter.
        let launch: fn() -> phoxal::Result<()> = <Countdown as LaunchedRuntime>::launch;
        let _ = launch;

        let spec = <phoxal_runtime_countdown::Adapter as RegisteredRuntime>::SPEC;
        assert_eq!(spec.period, ExecutionDuration::from_millis(20));
        assert_eq!(spec.timeout, ExecutionDuration::from_millis(100));
        assert_eq!(spec.init_timeout, ExecutionDuration::from_millis(1_000));
        phoxal_runtime_countdown::Adapter::retain_artifact_metadata();

        // The compiled input records carry the canonical operation
        // identities the launch manifest's artifact entry must mirror.
        for field in <countdown_api::Inputs as InputSet>::FIELDS {
            let signature = field.port_signature.expect("operations carry signatures");
            match field.name {
                "start" => {
                    assert_eq!(
                        signature.service,
                        "phoxal.tests.authoring.countdown.v1.Start"
                    );
                    assert_eq!(
                        signature.request,
                        "phoxal.tests.authoring.countdown.v1.StartRequest"
                    );
                    assert_eq!(
                        signature.response,
                        "phoxal.tests.authoring.countdown.v1.StartResponse"
                    );
                    assert_eq!(field.max_items, Some(16));
                    assert_eq!(field.max_bytes, Some(16_384));
                }
                "cancel" => {
                    assert_eq!(
                        signature.service,
                        "phoxal.tests.authoring.countdown.v1.Cancel"
                    );
                    assert_eq!(
                        signature.request,
                        "phoxal.tests.authoring.countdown.v1.CancelRequest"
                    );
                    assert_eq!(
                        signature.response,
                        "phoxal.tests.authoring.countdown.v1.CancelResponse"
                    );
                }
                other => panic!("unexpected input field {other}"),
            }
        }
        for field in <countdown_api::Outputs as OutputSet>::FIELDS {
            if field.name == "finished" {
                assert_eq!(field.max_items, Some(16));
                assert_eq!(field.max_bytes, Some(4_096));
            }
        }
        for field in <phoxal_runtime_countdown::Adapter as OutputBindings>::FIELDS {
            if field.name == "status" {
                assert!(field.bootstrap, "State<T> publishes before the first step");
            }
        }
    }
}
