use phoxal_supervisor_api::supervisor;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use phoxal::api;
use phoxal::prelude::*;
use phoxal::testing::{TestHarness, run_test_harness};
use phoxal_bus::{BusConfig, BusOwner, ExecutionId};
use tokio::sync::Notify;

static STEP_COUNT: AtomicUsize = AtomicUsize::new(0);
static STEP_STARTED: OnceLock<Notify> = OnceLock::new();

fn step_started() -> &'static Notify {
    STEP_STARTED.get_or_init(Notify::new)
}

#[derive(Default)]
struct SmokeState {
    steps: usize,
}

#[phoxal::service(id = "serialized-smoke", state = SmokeState)]
struct SerializedSmoke;

impl Participant for SerializedSmoke {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        ctx.query(supervisor::topic::owner().asset().get(), Self::query)?;
        Ok((SmokeState::default(), ()))
    }

    #[phoxal::step(hz = 20)]
    fn step(&self, _api: &Self::Api, _step: StepContext, state: &mut Self::State) -> Result<()> {
        if state.steps == 0 {
            step_started().notify_waiters();
        }
        state.steps += 1;
        STEP_COUNT.store(state.steps, Ordering::Release);
        Ok(())
    }
}

impl SerializedSmoke {
    fn query(
        &self,
        _api: &(),
        _query: QueryContext,
        _request: supervisor::asset::GetRequest,
        state: &mut SmokeState,
    ) -> QueryResult<supervisor::asset::GetResponse> {
        Ok(supervisor::asset::GetResponse::Found {
            bytes: (state.steps as u64).to_le_bytes().to_vec(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pending_query_reply_does_not_hold_serialized_steps() {
    STEP_COUNT.store(0, Ordering::Release);
    let first_step = step_started().notified();
    let launch = TestHarness::new("serialized-smoke")
        .expect("valid test participant")
        .with_query_reply_delay(Duration::from_millis(120));
    let participant =
        phoxal::bus::ParticipantId::new("serialized-smoke").expect("test participant id");
    let execution = ExecutionId::mint();
    let (owner, bus) = BusOwner::open(BusConfig::for_participant(
        execution,
        participant,
        Vec::new(),
    ))
    .await
    .expect("open in-process bus");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let runner_bus = bus.clone();
    let runner = async move {
        run_test_harness::<SerializedSmoke, _>(&runner_bus, launch, async {
            let _ = shutdown_rx.await;
        })
        .await
    };
    let smoke = async {
        tokio::time::timeout(Duration::from_secs(2), first_step)
            .await
            .expect("the scheduled step should start");

        let querier = Querier::new(
            bus.clone(),
            &supervisor::topic::client().asset().get(),
            Duration::from_secs(2),
        )
        .expect("create smoke querier");
        // The test harness deliberately stalls the reply transport for 120 ms.
        // Query handler evaluation and the eventual `IncomingQuery::reply*`
        // transport await are both outside the serialized owner; neither may
        // make the step cadence wait for a response consumer.
        let query_task = tokio::spawn({
            let querier = querier.clone();
            async move {
                querier
                    .query(supervisor::asset::GetRequest {
                        path: "smoke".to_string(),
                    })
                    .await
            }
        });
        let steps_before_pending_reply = STEP_COUNT.load(Ordering::Acquire);
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(
            STEP_COUNT.load(Ordering::Acquire) > steps_before_pending_reply,
            "step cadence must continue while a query reply is still in flight"
        );
        let response = query_task
            .await
            .expect("query task should not panic")
            .expect("the serialized query should answer");
        let supervisor::asset::GetResponse::Found { bytes } = response else {
            panic!("smoke query returned the wrong response variant");
        };
        let observed_steps =
            u64::from_le_bytes(bytes.try_into().expect("encoded step count")) as usize;
        assert!(
            observed_steps >= 1,
            "the query must observe a completed step"
        );

        shutdown_tx.send(()).expect("runner still listening");
    };
    let (runner_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(runner, smoke)
    })
    .await
    .expect("runner and smoke client should finish");
    runner_result.expect("runner should shut down cleanly");
    owner.close().await;
}
