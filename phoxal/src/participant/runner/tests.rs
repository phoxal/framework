use super::ShutdownController;
use super::event_loop::advance_step_deadline;
use super::lifecycle::{
    BusLease, ClockDisciplineLost, LoopExit, ParticipantFault, Runner, RunnerClock, RunnerTasks,
    StartOutcome, close_session_with_result,
};
use crate::bus::{RobotInstant, TimelineId};
use crate::participant::api::Participant;
use crate::participant::bus_log;
use crate::participant::clock::TimeUnsynchronized;
use crate::participant::clock::real::RealClock;
use crate::participant::context::SetupContext;
use crate::participant::managed::{
    ManagedTaskExit, ManagedTaskFailure, ManagedTaskPolicy, ManagedTasks,
};
use crate::participant::scheduler::AnyStepScheduler;
use phoxal_bus::{BusConfig, BusOwner};
use phoxal_runtime_contract::identity::ParticipantId;
use phoxal_runtime_contract::launch::ClockMode;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::Notify;

fn at(timeline: u64, ticks: u64) -> RobotInstant {
    RobotInstant::new(
        TimelineId::from_raw(timeline).expect("test timeline must be nonzero"),
        ticks,
    )
}

#[tokio::test]
async fn shutdown_request_remains_sticky_after_source_completes() {
    let mut shutdown = ShutdownController::new(std::future::ready(()));
    shutdown.wait().await;
    assert!(shutdown.is_requested());
    tokio::time::timeout(Duration::from_millis(10), shutdown.wait())
        .await
        .expect("a completed shutdown source must remain immediately observable");
}

#[test]
fn step_deadlines_skip_collapsed_ticks_and_saturate_instead_of_wrapping() {
    assert_eq!(
        advance_step_deadline(at(1, 10), Duration::from_nanos(10), 3),
        at(1, 50),
        "target 10 plus the fired period and 3 collapsed periods should resume at 50"
    );
    assert_eq!(
        advance_step_deadline(at(2, u64::MAX - 2), Duration::from_nanos(10), 3),
        at(2, u64::MAX)
    );
}

/// Both failing loop exits reach the operator as an actionable error naming
/// what went wrong, and the clock one keeps its reason as a value rather than
/// only in rendered text.
#[test]
fn loop_exits_report_actionable_failures() {
    let clock = LoopExit::ClockDisciplineLost(TimeUnsynchronized::ClockFault)
        .into_result()
        .expect_err("lost clock discipline is a failure");
    let fault = clock
        .downcast_ref::<ParticipantFault>()
        .expect("the primary result keeps its participant fault kind");
    let ParticipantFault::Clock(lost) = fault else {
        panic!("expected a clock fault");
    };
    assert_eq!(lost.reason, TimeUnsynchronized::ClockFault);
    assert_eq!(
        clock
            .source()
            .and_then(|source| source.downcast_ref::<ClockDisciplineLost>())
            .map(|lost| lost.reason),
        Some(TimeUnsynchronized::ClockFault),
        "the reason must survive as a value, not only in the message: {clock}"
    );
    assert_eq!(
        format!("{clock}"),
        "clock discipline lost: the host boot clock read failed or regressed",
        "the supervisor keeps this text as the failure evidence"
    );

    let step_source = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "motor link");
    let step =
        LoopExit::StepFailed(anyhow::Error::new(step_source).context("step transition failed"))
            .into_result()
            .expect_err("a step failure is terminal");
    assert!(matches!(
        step.downcast_ref::<ParticipantFault>(),
        Some(ParticipantFault::Step(_))
    ));
    assert!(
        step.chain()
            .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()),
        "step source evidence must survive the participant fault wrapper"
    );
    assert_eq!(format!("{step}"), "step failed: step transition failed");

    let reset = LoopExit::ResetFailed(anyhow::anyhow!("new world rejected"))
        .into_result()
        .expect_err("a reset failure is terminal");
    assert!(matches!(
        reset.downcast_ref::<ParticipantFault>(),
        Some(ParticipantFault::Reset(_))
    ));

    let panicked = LoopExit::ManagedTaskFaulted(ManagedTaskExit {
        name: "io-pump".to_string(),
        failure: ManagedTaskFailure::Panicked("serial port vanished".to_string()),
    })
    .into_result()
    .expect_err("a faulted managed task is a failure");
    assert_eq!(
        format!("{panicked}"),
        "managed task \"io-pump\" panicked: serial port vanished"
    );

    let task_source = std::io::Error::new(std::io::ErrorKind::TimedOut, "serial read");
    let task_error = LoopExit::ManagedTaskFaulted(ManagedTaskExit {
        name: "io-pump".to_string(),
        failure: ManagedTaskFailure::Error(
            anyhow::Error::new(task_source).context("serial read failed"),
        ),
    })
    .into_result()
    .expect_err("an operational task fault is a failure");
    assert!(matches!(
        task_error.downcast_ref::<ParticipantFault>(),
        Some(ParticipantFault::ManagedTask(_))
    ));
    assert!(
        task_error
            .chain()
            .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()),
        "managed-task source evidence must survive both wrappers"
    );

    let returned = LoopExit::ManagedTaskFaulted(ManagedTaskExit {
        name: "io-pump".to_string(),
        failure: ManagedTaskFailure::Returned,
    })
    .into_result()
    .expect_err("a faulted managed task is a failure");
    assert_eq!(
        format!("{returned}"),
        "managed task \"io-pump\" exited unexpectedly"
    );

    assert!(LoopExit::ShutdownRequested.into_result().is_ok());
}

/// A Critical task that fails while the bus-side Ready declaration is
/// awaiting must win the boundary race, so no Ready token is accepted.
#[tokio::test(start_paused = true)]
async fn ready_declaration_race_prefers_a_task_failure() {
    let (trigger, triggered) = tokio::sync::oneshot::channel();
    let mut tasks = ManagedTasks::default();
    tasks.spawn(
        "declaration-race",
        ManagedTaskPolicy::Critical,
        async move {
            triggered.await.expect("the declaration triggers the task");
            Err::<(), _>(anyhow::anyhow!(
                "setup task failed during Ready declaration"
            ))
        },
    );

    let declaration = async move {
        trigger.send(()).expect("the task is still supervised");
        tokio::task::yield_now().await;
        Ok::<(), ()>(())
    };
    let failure = tokio::select! {
        biased;
        exit = tasks.next_unexpected_exit() => Some(exit),
        _ = declaration => None,
    };
    let failure = failure.expect("task failure must preempt Ready acquisition");
    assert_eq!(failure.name, "declaration-race");
    let ManagedTaskFailure::Error(error) = failure.failure else {
        panic!("expected the operational task error");
    };
    assert_eq!(
        error.to_string(),
        "setup task failed during Ready declaration"
    );
}

static HANGING_SETUP_STARTED: OnceLock<Notify> = OnceLock::new();

fn hanging_setup_started() -> &'static Notify {
    HANGING_SETUP_STARTED.get_or_init(Notify::new)
}

/// A stop received while setup is still awaiting must cancel setup-owned tasks
/// and return before Ready. The setup barrier makes sure the shutdown trigger
/// cannot win merely because the biased select was polled before setup started.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_hanging_setup_never_reaches_ready() {
    #[phoxal::service(id = "hanging-startup", state = ())]
    struct HangingStartup;

    impl Participant for HangingStartup {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            hanging_setup_started().notify_one();
            std::future::pending().await
        }
    }

    let participant_id = ParticipantId::new("hanging-startup").expect("valid participant id");
    let (owner, bus) = BusOwner::open(BusConfig::for_participant(
        phoxal_runtime_contract::identity::ExecutionId::mint(),
        participant_id.clone(),
        Vec::new(),
    ))
    .await
    .expect("open in-process bus");
    let (scheduler, clock_handle) = AnyStepScheduler::for_clock_mode(
        ClockMode::Real,
        None,
        Some(RobotInstant::new(
            TimelineId::from_raw(1).expect("valid timeline"),
            0,
        )),
    )
    .expect("real scheduler");
    assert!(clock_handle.is_none());
    let (bus_logs, bus_log_task) = bus_log::attach(bus.clone(), participant_id.as_str());
    let clock = RealClock::new(phoxal_runtime_contract::origin::ExecutionOrigin::mint())
        .expect("current-boot origin");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let setup_started = hanging_setup_started().notified();
    let start_task = tokio::spawn(async move {
        let mut shutdown = ShutdownController::new(async move {
            let _ = shutdown_rx.await;
        });
        Runner::<HangingStartup, RealClock>::start(
            super::lifecycle::StartInputs {
                bus,
                session: BusLease::Owned(owner),
                participant_id,
                shutdown_grace: Duration::from_millis(100),
                bundle: None,
                config: (),
                clock: RunnerClock::Delegated(clock),
                scheduler,
                schedule: None,
                clock_mode: ClockMode::Real,
                tasks: RunnerTasks {
                    simulation_clock: None,
                    bus_log: bus_log_task,
                    query_reply_delay: None,
                },
            },
            &mut shutdown,
        )
        .await
    });
    setup_started.await;
    shutdown_tx
        .send(())
        .expect("startup shutdown trigger is pending");

    let result = start_task
        .await
        .expect("startup task must finish after shutdown trigger");
    let StartOutcome::Terminal {
        result,
        deadline,
        session,
    } = result
    else {
        panic!("shutdown during setup must terminate before Ready");
    };
    result.expect("startup cancellation should be clean");
    close_session_with_result(Ok::<(), anyhow::Error>(()), session, deadline)
        .await
        .expect("bus close after cancelled setup");
    bus_logs.shutdown();
}
