use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use phoxal::__private::{ExecutionOrigin, ParticipantLaunch};
use phoxal::api;
use phoxal::prelude::*;
use phoxal_bus::{Bus, BusConfig};
use tokio::sync::Notify;

static STEP_COUNT: AtomicUsize = AtomicUsize::new(0);
static STEP_STARTED: OnceLock<Notify> = OnceLock::new();
static STEP_FINISHED: OnceLock<Notify> = OnceLock::new();

fn step_started() -> &'static Notify {
    STEP_STARTED.get_or_init(Notify::new)
}

fn step_finished() -> &'static Notify {
    STEP_FINISHED.get_or_init(Notify::new)
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
        ctx.query(api::topic::owner().supervisor().asset().get(), Self::query)
            .await?;
        Ok((SmokeState::default(), ()))
    }

    #[phoxal::step(hz = 20)]
    async fn step(
        &self,
        _api: &Self::Api,
        _step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        if state.steps == 0 {
            step_started().notify_waiters();
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        state.steps += 1;
        STEP_COUNT.store(state.steps, Ordering::Release);
        step_finished().notify_waiters();
        Ok(())
    }
}

impl SerializedSmoke {
    async fn query(
        &self,
        _api: &(),
        _request: api::supervisor::asset::GetRequest,
        state: &mut SmokeState,
    ) -> QueryResult<api::supervisor::asset::GetResponse> {
        tokio::time::sleep(Duration::from_millis(120)).await;
        Ok(api::supervisor::asset::GetResponse::Found {
            bytes: (state.steps as u64).to_le_bytes().to_vec(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_query_waits_for_an_in_flight_step_and_stepping_resumes_afterward() {
    STEP_COUNT.store(0, Ordering::Release);
    let first_step = step_started().notified();
    let launch = ParticipantLaunch::local("serialized-smoke", "smoke")
        .with_execution_origin(ExecutionOrigin::mint());
    let bus = Bus::open(BusConfig {
        execution: launch.execution,
        participant: launch.participant_id.clone(),
        connect_endpoints: Vec::new(),
    })
    .await
    .expect("open in-process bus");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let runner_bus = bus.clone();
    let runner = async move {
        phoxal::__private::run_with_bus::<SerializedSmoke, _>(&runner_bus, launch, async {
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
            &api::topic::client().supervisor().asset().get(),
            Duration::from_secs(2),
        )
        .expect("create smoke querier");
        let began = Instant::now();
        let response = querier
            .query(api::supervisor::asset::GetRequest {
                path: "smoke".to_string(),
            })
            .await
            .expect("the serialized query should answer");
        let latency = began.elapsed();

        let api::supervisor::asset::GetResponse::Found { bytes } = response else {
            panic!("smoke query returned the wrong response variant");
        };
        let observed_steps =
            u64::from_le_bytes(bytes.try_into().expect("encoded step count")) as usize;
        assert!(
            observed_steps >= 1,
            "the query must observe the complete in-flight step"
        );
        assert!(
            latency >= Duration::from_millis(220),
            "query latency must include the in-flight step and handler, got {latency:?}"
        );

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let next_step = step_finished().notified();
                if STEP_COUNT.load(Ordering::Acquire) > observed_steps {
                    break;
                }
                next_step.await;
            }
        })
        .await
        .expect("stepping should resume after the query");

        shutdown_tx.send(()).expect("runner still listening");
    };
    let (runner_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(runner, smoke)
    })
    .await
    .expect("runner and smoke client should finish");
    runner_result.expect("runner should shut down cleanly");
    bus.close().await.expect("close smoke bus");
}
