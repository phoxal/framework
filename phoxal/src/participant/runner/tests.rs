use super::ShutdownController;
use super::event_loop::advance_step_deadline;
use super::lifecycle::{
    BusLease, ClockDisciplineLost, LoopExit, ParticipantFault, ReadyDomainFence, Runner,
    RunnerClock, RunnerTasks, StartOutcome, close_session_with_result, fence_ready_domain,
    runner_clock, runner_clock_for_domain, scheduler_for_domain,
};
use super::startup::DomainSubscription;
use crate::bus::{
    BusConfig, BusFault, BusOwner, EventPublisher, ParticipantReadyEvents, ParticipantReadyStatus,
    RobotInstant, StatePublisher, StepToken, StreamPublisher, StreamReceiver, TimelineId,
};
use crate::identity::ParticipantId;
use crate::participant::api::Participant;
use crate::participant::bus_log;
use crate::participant::clock::real::RealClock;
use crate::participant::clock::test::TestClock;
use crate::participant::clock::{ClockMode, ClockReading, ClockSource, TimeUnsynchronized};
use crate::participant::context::{SetupContext, SetupSource, StepContext};
use crate::participant::managed::{
    ManagedTaskExit, ManagedTaskFailure, ManagedTaskPolicy, ManagedTasks,
};
use crate::participant::scheduler::AnyStepScheduler;
use crate::participant::scheduler::simulation::{SimulationClockAdvance, SimulationClockHandle};
use crate::supervisor::api;
use crate::supervisor::api::time_domain::{TimeDomain, TimeDomainStream, TimeMode};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::Notify;

fn at(timeline: u64, ticks: u64) -> RobotInstant {
    RobotInstant::new(
        TimelineId::from_raw(timeline).expect("test timeline must be nonzero"),
        ticks,
    )
}

fn test_timeline() -> TimelineId {
    TimelineId::from_raw(1).expect("test timeline must be nonzero")
}

/// A replacement buffered by the already-subscribed stream while Ready is
/// being acquired revokes that lease and requires startup to reconfigure first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ready_domain_fence_reconfigures_a_buffered_replacement() {
    let participant = ParticipantId::new("ready-domain-fence").expect("valid participant id");
    let (owner, bus) = BusOwner::open(BusConfig::for_participant(
        crate::identity::ExecutionId::mint(),
        participant,
        Vec::new(),
    ))
    .await
    .expect("the in-process bus opens");
    let updates =
        StreamReceiver::<TimeDomainStream>::new(&bus, &api::topics().time_domain().client())
            .await
            .expect("the Ready fence subscribes");
    let publisher = StreamPublisher::new(bus.clone(), &api::topics().time_domain().owner())
        .expect("the supervisor stream publisher attaches");
    let initial = TimeDomain {
        revision: 10,
        timeline: test_timeline(),
        mode: TimeMode::Monotonic,
    };
    let replacement = TimeDomain {
        revision: 11,
        timeline: TimelineId::from_raw(2).expect("a replacement timeline"),
        mode: TimeMode::Simulated,
    };
    publisher
        .send(TimeDomainStream {
            domain: replacement,
        })
        .expect("the replacement is admitted");
    let mut domain = Some(DomainSubscription {
        current: initial,
        updates,
    });
    let transitions = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match fence_ready_domain(&mut domain)
                .expect("the fence reconciles the buffered replacement")
            {
                ReadyDomainFence::Stable => tokio::task::yield_now().await,
                ReadyDomainFence::Reconfigure(transitions) => break transitions,
            }
        }
    })
    .await
    .expect("the replacement reaches the Ready-fence subscription");
    assert_eq!(transitions, vec![(initial, replacement)]);
    let ReadyDomainFence::Stable =
        fence_ready_domain(&mut domain).expect("the drained fence remains healthy")
    else {
        panic!("the drained fence must be stable");
    };

    drop(domain);
    drop(publisher);
    let _ = owner.close().await;
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

/// A stepless real participant keeps its host clock, so its recurring beat
/// reads real robot time instead of faulting on a clock that was never there.
/// Having no cadence is not having no time.
#[test]
fn a_stepless_real_participant_keeps_its_host_clock() {
    let (scheduler, handle) = AnyStepScheduler::for_clock_mode(ClockMode::Real, None, None)
        .expect("a stepless real participant builds without a scheduler");
    assert!(handle.is_none());
    assert!(matches!(scheduler, AnyStepScheduler::Disabled));

    assert!(matches!(
        runner_clock(&scheduler, Some(RealClock::new(test_timeline()))),
        Ok(RunnerClock::Delegated(_))
    ));
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

    let bus = LoopExit::BusFaulted(BusFault::WorkerExited {
        worker: "subscription:drive/target".to_string(),
    })
    .into_result()
    .expect_err("an owner-owned bus worker exit is terminal");
    assert!(matches!(
        bus.downcast_ref::<ParticipantFault>(),
        Some(ParticipantFault::Bus(BusFault::WorkerExited { .. }))
    ));
    assert!(format!("{bus}").contains("bus transport failed"));

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
static DOMAIN_SETUP_STARTED: OnceLock<Notify> = OnceLock::new();
static DOMAIN_SETUP_RELEASE: OnceLock<Notify> = OnceLock::new();
static DOMAIN_SETUP_RESETS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SlowServiceObservation {
    instant: RobotInstant,
    step_index: u64,
    missed_ticks: u32,
}

struct SlowServiceState {
    observations: tokio::sync::mpsc::Sender<SlowServiceObservation>,
    first_step_release: Option<std::sync::mpsc::Receiver<()>>,
}

static SLOW_SERVICE_FIXTURE: OnceLock<Mutex<Option<SlowServiceState>>> = OnceLock::new();
static SLOW_SERVICE_RESETS: AtomicUsize = AtomicUsize::new(0);

fn slow_service_fixture() -> &'static Mutex<Option<SlowServiceState>> {
    SLOW_SERVICE_FIXTURE.get_or_init(|| Mutex::new(None))
}

#[phoxal::service(id = "slow-live-service", state = SlowServiceState)]
struct SlowLiveService;

impl Participant for SlowLiveService {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> crate::Result<(Self::State, Self::Api)> {
        let state = slow_service_fixture()
            .lock()
            .expect("the slow-service fixture lock is healthy")
            .take()
            .expect("the test installed one slow-service fixture");
        Ok((state, ()))
    }

    #[phoxal::step(hz = 100)]
    fn step(
        &self,
        _api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> crate::Result<()> {
        state
            .observations
            .try_send(SlowServiceObservation {
                instant: step.now(),
                step_index: step.step_index,
                missed_ticks: step.missed_ticks,
            })
            .expect("the test still observes service steps");
        if let Some(release) = state.first_step_release.take() {
            release
                .recv()
                .expect("the test releases the deliberately slow first invocation");
        }
        Ok(())
    }

    fn reset(
        &self,
        _ctx: crate::participant::context::ResetContext,
        _api: &Self::Api,
        _state: &mut Self::State,
    ) -> crate::Result<()> {
        SLOW_SERVICE_RESETS.fetch_add(1, Ordering::Release);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WorldProbeSnapshot {
    completed_transitions: u64,
    output_count: u64,
    step_event_count: u64,
}

enum WorldProbeCommand {
    SetRunning {
        running: bool,
        reply: tokio::sync::oneshot::Sender<WorldProbeSnapshot>,
    },
    Transition {
        reply: tokio::sync::oneshot::Sender<crate::Result<Option<RobotInstant>>>,
    },
    Snapshot {
        reply: tokio::sync::oneshot::Sender<WorldProbeSnapshot>,
    },
    Stop,
}

fn read_test_clock(clock: &TestClock) -> RobotInstant {
    let ClockReading::Synchronized(instant) = clock.read() else {
        panic!("the deterministic monotonic clock stays synchronized");
    };
    instant
}

fn advance_test_monotonic_time(
    clock: &TestClock,
    cadence: &SimulationClockHandle,
    delta: Duration,
) -> RobotInstant {
    clock.advance(delta);
    let instant = read_test_clock(clock);
    assert_eq!(
        cadence.advance(instant),
        SimulationClockAdvance::Advanced,
        "each host-monotonic advance releases at most one collapsed service invocation"
    );
    instant
}

fn spawn_world_probe(
    bus: &crate::bus::BusHandle,
    clock: TestClock,
) -> (
    tokio::sync::mpsc::Sender<WorldProbeCommand>,
    tokio::task::JoinHandle<()>,
) {
    let state = StatePublisher::new(bus.clone(), &crate::api::topics().drive().state().owner())
        .expect("the world probe binds one typed simulator output");
    let step = EventPublisher::new(
        bus.clone(),
        &crate::simulation::api::topics().step().owner(),
    )
    .expect("the world probe binds passive StepEvent progress");
    let (commands, mut received) = tokio::sync::mpsc::channel(8);
    let task = tokio::spawn(async move {
        let mut running = true;
        let mut snapshot = WorldProbeSnapshot::default();
        while let Some(command) = received.recv().await {
            match command {
                WorldProbeCommand::SetRunning {
                    running: requested,
                    reply,
                } => {
                    running = requested;
                    let _ = reply.send(snapshot);
                }
                WorldProbeCommand::Transition { reply } => {
                    if !running {
                        let _ = reply.send(Ok(None));
                        continue;
                    }
                    let instant = read_test_clock(&clock);
                    let token = StepToken::mint(instant);
                    let next = snapshot.completed_transitions.saturating_add(1);
                    let result = state
                        .publish(
                            &token,
                            crate::api::drive::State::Stopped {
                                target: crate::api::drive::Target::stopped(),
                                reason: crate::api::drive::StopReason::Fault,
                            },
                        )
                        .and_then(|()| {
                            step.publish(&token, crate::simulation::api::StepEvent { index: next })
                        })
                        .map(|()| {
                            snapshot.completed_transitions = next;
                            snapshot.output_count = snapshot.output_count.saturating_add(1);
                            snapshot.step_event_count = snapshot.step_event_count.saturating_add(1);
                            Some(instant)
                        })
                        .map_err(anyhow::Error::from);
                    let _ = reply.send(result);
                }
                WorldProbeCommand::Snapshot { reply } => {
                    let _ = reply.send(snapshot);
                }
                WorldProbeCommand::Stop => break,
            }
        }
    });
    (commands, task)
}

async fn set_world_probe_running(
    commands: &tokio::sync::mpsc::Sender<WorldProbeCommand>,
    running: bool,
) -> WorldProbeSnapshot {
    let (reply, result) = tokio::sync::oneshot::channel();
    commands
        .send(WorldProbeCommand::SetRunning { running, reply })
        .await
        .expect("the independent world probe is running");
    result.await.expect("the world probe acknowledges motion")
}

async fn advance_world_probe(
    commands: &tokio::sync::mpsc::Sender<WorldProbeCommand>,
) -> crate::Result<Option<RobotInstant>> {
    let (reply, result) = tokio::sync::oneshot::channel();
    commands
        .send(WorldProbeCommand::Transition { reply })
        .await
        .expect("the independent world probe is running");
    result
        .await
        .expect("the world probe answers one transition request")
}

async fn world_probe_snapshot(
    commands: &tokio::sync::mpsc::Sender<WorldProbeCommand>,
) -> WorldProbeSnapshot {
    let (reply, result) = tokio::sync::oneshot::channel();
    commands
        .send(WorldProbeCommand::Snapshot { reply })
        .await
        .expect("the independent world probe is running");
    result.await.expect("the world probe returns its snapshot")
}

/// Live world progress and monotonic service cadence are independent tasks.
///
/// The service's first synchronous invocation is held open while the world
/// admits three typed outputs and three passive StepEvents on the same bus.
/// Host time continues to advance, so releasing the service produces one
/// collapsed invocation with ordinary `missed_ticks`, never a catch-up storm.
/// Pausing then suppresses only world production: two on-cadence service
/// invocations still run on the original timeline, with no reset, and resume
/// admits the next output and StepEvent immediately.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_monotonic_service_and_live_pause_are_independent() {
    SLOW_SERVICE_RESETS.store(0, Ordering::Release);
    let (observed, mut observations) = tokio::sync::mpsc::channel(8);
    let (release_first, first_step_release) = std::sync::mpsc::channel();
    *slow_service_fixture()
        .lock()
        .expect("the slow-service fixture lock is healthy") = Some(SlowServiceState {
        observations: observed,
        first_step_release: Some(first_step_release),
    });

    let participant_id =
        ParticipantId::new("slow-live-service").expect("the participant id is valid");
    let (owner, bus) = BusOwner::open(BusConfig::for_participant(
        crate::identity::ExecutionId::mint(),
        participant_id.clone(),
        Vec::new(),
    ))
    .await
    .expect("the shared in-process bus opens");
    let clock = TestClock::new();
    let timeline = clock.timeline();
    let schedule = SlowLiveService::__step_schedule().expect("the test service has a cadence");
    let (scheduler, cadence, mut runner_started) =
        AnyStepScheduler::test_monotonic(schedule, RobotInstant::new(timeline, 0));
    let (bus_logs, bus_log_task) = bus_log::attach(bus.clone());
    let mut startup_shutdown = ShutdownController::new(std::future::pending());
    let outcome = Runner::<SlowLiveService, TestClock>::start(
        super::lifecycle::StartInputs {
            bus: bus.clone(),
            session: BusLease::Borrowed,
            participant_id,
            shutdown_grace: Duration::from_secs(1),
            source: SetupSource::Harness,
            domain: None,
            attachment: None,
            config: (),
            clock: RunnerClock::Delegated(clock.clone()),
            scheduler,
            schedule: Some(schedule),
            clock_mode: ClockMode::Real,
            tasks: RunnerTasks {
                simulation_clock: None,
                bus_log: bus_log_task,
                query_reply_delay: None,
            },
        },
        &mut startup_shutdown,
    )
    .await;
    let StartOutcome::Ready(runner) = outcome else {
        panic!("the deterministic monotonic service reaches Ready");
    };
    let (world, world_task) = spawn_world_probe(&bus, clock.clone());
    let (shutdown, shutdown_requested) = tokio::sync::oneshot::channel();
    let runner_task = tokio::spawn(async move {
        let mut shutdown = ShutdownController::new(async move {
            let _ = shutdown_requested.await;
        });
        runner.run(&mut shutdown).await
    });

    if !*runner_started.borrow_and_update() {
        runner_started
            .changed()
            .await
            .expect("the deterministic monotonic scheduler remains live");
    }

    advance_test_monotonic_time(&clock, &cadence, Duration::from_millis(10));
    let first = tokio::time::timeout(Duration::from_secs(2), observations.recv())
        .await
        .expect("the first service invocation starts")
        .expect("the service observation channel stays open");
    assert_eq!(
        first,
        SlowServiceObservation {
            instant: RobotInstant::new(timeline, 10_000_000),
            step_index: 0,
            missed_ticks: 0,
        }
    );

    let mut world_instants = Vec::new();
    for _ in 0..3 {
        let instant = advance_test_monotonic_time(&clock, &cadence, Duration::from_millis(12));
        assert_eq!(
            advance_world_probe(&world)
                .await
                .expect("world output admission stays non-blocking"),
            Some(instant),
            "world progress completes while the service invocation is still held"
        );
        world_instants.push(instant);
    }
    assert_eq!(
        world_probe_snapshot(&world).await,
        WorldProbeSnapshot {
            completed_transitions: 3,
            output_count: 3,
            step_event_count: 3,
        }
    );

    release_first
        .send(())
        .expect("the first service invocation is still blocked");
    let collapsed = tokio::time::timeout(Duration::from_secs(2), observations.recv())
        .await
        .expect("the service catches up once")
        .expect("the service observation channel stays open");
    assert_eq!(collapsed.instant, RobotInstant::new(timeline, 46_000_000));
    assert_eq!(collapsed.step_index, 1);
    assert_eq!(
        collapsed.missed_ticks, 2,
        "the 20 ms target observes 26 ms of overrun as two collapsed periods"
    );
    assert!(
        world_instants
            .iter()
            .all(|instant| instant.timeline() == timeline)
    );

    let paused_at = set_world_probe_running(&world, false).await;
    assert_eq!(paused_at.completed_transitions, 3);
    advance_test_monotonic_time(&clock, &cadence, Duration::from_millis(4));
    let at_fifty = tokio::time::timeout(Duration::from_secs(2), observations.recv())
        .await
        .expect("service cadence reaches 50 ms while the world is paused")
        .expect("the service observation channel stays open");
    advance_test_monotonic_time(&clock, &cadence, Duration::from_millis(10));
    let at_sixty = tokio::time::timeout(Duration::from_secs(2), observations.recv())
        .await
        .expect("service cadence reaches 60 ms while the world is paused")
        .expect("the service observation channel stays open");
    for observation in [at_fifty, at_sixty] {
        assert_eq!(observation.instant.timeline(), timeline);
        assert_eq!(observation.missed_ticks, 0);
    }
    assert_eq!(
        advance_world_probe(&world)
            .await
            .expect("a paused transition request is handled"),
        None,
        "pause suppresses both the simulator output and StepEvent"
    );
    assert_eq!(world_probe_snapshot(&world).await, paused_at);

    set_world_probe_running(&world, true).await;
    let resumed_at = advance_test_monotonic_time(&clock, &cadence, Duration::from_millis(2));
    assert_eq!(
        advance_world_probe(&world)
            .await
            .expect("the first resumed publication is admitted"),
        Some(resumed_at)
    );
    assert_eq!(resumed_at.timeline(), timeline);
    assert_eq!(
        world_probe_snapshot(&world).await,
        WorldProbeSnapshot {
            completed_transitions: 4,
            output_count: 4,
            step_event_count: 4,
        }
    );
    assert_eq!(
        SLOW_SERVICE_RESETS.load(Ordering::Acquire),
        0,
        "Live pause and resume never replace the monotonic timeline"
    );

    world
        .send(WorldProbeCommand::Stop)
        .await
        .expect("the world probe is still running");
    world_task.await.expect("the world probe stops cleanly");
    shutdown
        .send(())
        .expect("the participant runner still awaits shutdown");
    runner_task
        .await
        .expect("the runner task joins")
        .expect("the participant shuts down cleanly");
    let _ = owner.close().await;
    bus_logs.shutdown();
}

fn hanging_setup_started() -> &'static Notify {
    HANGING_SETUP_STARTED.get_or_init(Notify::new)
}

fn domain_setup_started() -> &'static Notify {
    DOMAIN_SETUP_STARTED.get_or_init(Notify::new)
}

fn domain_setup_release() -> &'static Notify {
    DOMAIN_SETUP_RELEASE.get_or_init(Notify::new)
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
        crate::identity::ExecutionId::mint(),
        participant_id.clone(),
        Vec::new(),
    ))
    .await
    .expect("open in-process bus");
    let (scheduler, clock_handle) = AnyStepScheduler::for_clock_mode(
        ClockMode::Real,
        None,
        Some(RobotInstant::new(test_timeline(), 0)),
    )
    .expect("real scheduler");
    assert!(clock_handle.is_none());
    let (bus_logs, bus_log_task) = bus_log::attach(bus.clone());
    let clock = RealClock::new(test_timeline());
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
                source: SetupSource::Harness,
                domain: None,
                attachment: None,
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

/// A domain replacement delivered while setup is pending is reconciled and
/// reset before the participant can acquire Ready.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_domain_change_during_setup_resets_before_ready() {
    #[phoxal::service(id = "domain-transition-startup", state = ())]
    struct DomainTransitionStartup;

    impl Participant for DomainTransitionStartup {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            domain_setup_started().notify_one();
            domain_setup_release().notified().await;
            Ok(((), ()))
        }

        fn reset(
            &self,
            _ctx: crate::participant::context::ResetContext,
            _api: &Self::Api,
            _state: &mut Self::State,
        ) -> crate::Result<()> {
            DOMAIN_SETUP_RESETS.fetch_add(1, Ordering::Release);
            Ok(())
        }
    }

    DOMAIN_SETUP_RESETS.store(0, Ordering::Release);
    let participant_id =
        ParticipantId::new("domain-transition-startup").expect("a valid participant id");
    let (owner, bus) = BusOwner::open(BusConfig::for_participant(
        crate::identity::ExecutionId::mint(),
        participant_id.clone(),
        Vec::new(),
    ))
    .await
    .expect("the in-process bus opens");
    let updates =
        StreamReceiver::<TimeDomainStream>::new(&bus, &api::topics().time_domain().client())
            .await
            .expect("the runner subscribes before setup");
    let delivery =
        StreamReceiver::<TimeDomainStream>::new(&bus, &api::topics().time_domain().client())
            .await
            .expect("the delivery observer subscribes");
    let publisher = StreamPublisher::new(bus.clone(), &api::topics().time_domain().owner())
        .expect("the supervisor stream publisher attaches");
    let initial = TimeDomain {
        revision: 0,
        timeline: test_timeline(),
        mode: TimeMode::Monotonic,
    };
    let replacement = TimeDomain {
        revision: 1,
        timeline: TimelineId::from_raw(2).expect("a replacement timeline"),
        mode: TimeMode::Simulated,
    };
    let second_replacement = TimeDomain {
        revision: 2,
        timeline: TimelineId::from_raw(3).expect("a second replacement timeline"),
        mode: TimeMode::Monotonic,
    };
    let simulation_clock = SimulationClockHandle::source();
    let initial_clock = RealClock::new(initial.timeline);
    let scheduler = scheduler_for_domain(
        ClockMode::Real,
        None,
        initial_clock.read().instant(),
        &simulation_clock,
    )
    .expect("the initial monotonic scheduler builds");
    let clock = runner_clock_for_domain::<RealClock>(&scheduler, initial)
        .expect("the initial runner clock builds");
    let (bus_logs, bus_log_task) = bus_log::attach(bus.clone());
    let setup_started = domain_setup_started().notified();
    let start_task = tokio::spawn(async move {
        let mut shutdown = ShutdownController::new(std::future::pending());
        Runner::<DomainTransitionStartup, RealClock>::start(
            super::lifecycle::StartInputs {
                bus,
                session: BusLease::Owned(owner),
                participant_id,
                shutdown_grace: Duration::from_millis(100),
                source: SetupSource::Harness,
                domain: Some(DomainSubscription {
                    current: initial,
                    updates,
                }),
                attachment: None,
                config: (),
                clock,
                scheduler,
                schedule: None,
                clock_mode: ClockMode::Real,
                tasks: RunnerTasks {
                    simulation_clock: Some(simulation_clock),
                    bus_log: bus_log_task,
                    query_reply_delay: None,
                },
            },
            &mut shutdown,
        )
        .await
    });
    setup_started.await;
    publisher
        .send(TimeDomainStream {
            domain: replacement,
        })
        .expect("the replacement is admitted");
    publisher
        .send(TimeDomainStream {
            domain: second_replacement,
        })
        .expect("the second replacement is admitted");
    for expected in [replacement, second_replacement] {
        let delivered = tokio::time::timeout(Duration::from_secs(2), delivery.recv())
            .await
            .expect("the replacement reaches subscribers")
            .expect("the replacement decodes");
        assert_eq!(delivered.body.domain, expected);
    }
    domain_setup_release().notify_one();

    let outcome = start_task.await.expect("the startup task returns");
    let StartOutcome::Ready(runner) = outcome else {
        panic!("a healthy startup must reach Ready after its reset");
    };
    assert_eq!(
        DOMAIN_SETUP_RESETS.load(Ordering::Acquire),
        2,
        "every queued replacement reset must complete before Ready"
    );
    let mut shutdown = ShutdownController::new(std::future::ready(()));
    runner
        .run(&mut shutdown)
        .await
        .expect("the runner shuts down cleanly");
    bus_logs.shutdown();
}

static BUS_FAULT_SHUTDOWN_CALLED: AtomicBool = AtomicBool::new(false);

#[phoxal::service(id = "transport-fault-lifecycle", state = ())]
struct TransportFaultLifecycle;

impl Participant for TransportFaultLifecycle {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> crate::Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }

    async fn shutdown(&self, _api: &Self::Api, _state: &mut Self::State) -> crate::Result<()> {
        BUS_FAULT_SHUTDOWN_CALLED.store(true, Ordering::Release);
        Ok(())
    }
}

async fn wait_for_ready_status(events: &ParticipantReadyEvents, status: ParticipantReadyStatus) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            while let Some(event) = events.try_recv() {
                if event.status == status {
                    return;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the Ready lifecycle event must be observable");
}

async fn assert_owner_worker_failure_reaches_lifecycle(
    worker: &str,
    abort: impl FnOnce(&crate::bus::BusHandle) -> crate::bus::Result<()>,
) {
    BUS_FAULT_SHUTDOWN_CALLED.store(false, Ordering::Release);
    let participant_id =
        ParticipantId::new("transport-fault-lifecycle").expect("valid participant id");
    let (owner, bus) = BusOwner::open(BusConfig::for_participant(
        crate::identity::ExecutionId::mint(),
        participant_id.clone(),
        Vec::new(),
    ))
    .await
    .expect("open in-process bus");
    let ready_events = bus
        .participant_ready_events()
        .await
        .expect("observe exact Ready changes");
    let (scheduler, clock_handle) = AnyStepScheduler::for_clock_mode(ClockMode::Real, None, None)
        .expect("a stepless real participant needs no scheduler");
    assert!(clock_handle.is_none());
    let (bus_logs, bus_log_task) = bus_log::attach(bus.clone());
    let mut shutdown = ShutdownController::new(std::future::pending());

    let outcome = Runner::<TransportFaultLifecycle, RealClock>::start(
        super::lifecycle::StartInputs {
            bus: bus.clone(),
            session: BusLease::Owned(owner),
            participant_id,
            shutdown_grace: Duration::from_secs(1),
            source: SetupSource::Harness,
            domain: None,
            attachment: None,
            config: (),
            clock: RunnerClock::Delegated(RealClock::new(test_timeline())),
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
    .await;
    let StartOutcome::Ready(runner) = outcome else {
        panic!("healthy setup must acquire Ready before the injected failure");
    };
    wait_for_ready_status(&ready_events, ParticipantReadyStatus::Ready).await;

    abort(&bus).expect("the running owner has the selected transport worker");
    let error = runner
        .run(&mut shutdown)
        .await
        .expect_err("an owner-owned drain failure is terminal");
    assert!(matches!(
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ParticipantFault>()),
        Some(ParticipantFault::Bus(BusFault::WorkerJoin { worker: observed, .. }))
            if observed == worker
    ));
    assert!(BUS_FAULT_SHUTDOWN_CALLED.load(Ordering::Acquire));
    wait_for_ready_status(&ready_events, ParticipantReadyStatus::Lost).await;
    bus_logs.shutdown();
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_drain_failure_revokes_ready_runs_shutdown_and_returns_bus_fault() {
    assert_owner_worker_failure_reaches_lifecycle("outbound-drain", |bus| {
        bus.__test_abort_outbound_drain()
    })
    .await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_reaper_failure_revokes_ready_runs_shutdown_and_returns_bus_fault() {
    assert_owner_worker_failure_reaches_lifecycle("bus-worker-reaper", |bus| {
        bus.__test_abort_worker_reaper()
    })
    .await;
}
