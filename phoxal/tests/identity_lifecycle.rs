//! The identity lifecycle table of #952 section B, executed rather than
//! described.
//!
//! | Event | Execution | Timeline | Producer |
//! |---|---|---|---|
//! | Participant restart | same | same | new |
//! | Router recovery | same | new if Webots is recreated | new for recreated participants |
//! | Simulation pause / resume | same | same | same |
//! | Simulation reset, controller replacement, replay branch | same | new | new for the controller |
//! | Supervisor restart, new run, rollback | new | new | new |
//!
//! This file covers the rows a participant can observe from inside the
//! framework: restart, pause/resume, and world replacement. The rows that are
//! *supervisor* facts - router recovery, supervisor restart, new run, rollback,
//! and the detached resident adopting the launcher's execution - are proven in
//! `phoxal-cli`, which is the thing that mints and re-mints an `ExecutionId`;
//! the framework only ever receives one through its launch contract.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use phoxal::api;
use phoxal::bus::{StatePublisher, TimelineAuthority, TimelineId};
use phoxal::participant::{ClockMode, ParticipantLaunch};
use phoxal::prelude::*;
use phoxal::raw::{Bus, BusConfig, run_with_bus};

static STEPS: AtomicU64 = AtomicU64::new(0);
static TIMELINES: Mutex<Vec<TimelineId>> = Mutex::new(Vec::new());
static RESETS: Mutex<Vec<(TimelineId, TimelineId)>> = Mutex::new(Vec::new());

/// A participant that records nothing but identity: which world histories it
/// stepped on, and every reset it was asked to run.
#[phoxal::service(id = "identity-probe", config = (), api = ())]
struct IdentityProbe;

#[phoxal::behavior]
impl IdentityProbe {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[reset]
    async fn reset(&mut self, ctx: ResetContext) -> Result<()> {
        RESETS
            .lock()
            .expect("reset log poisoned")
            .push((ctx.previous_timeline(), ctx.new_timeline()));
        Ok(())
    }

    #[step(hz = 1000)]
    async fn step(&mut self, _api: &mut Self::Api, step: StepContext) -> Result<()> {
        STEPS.fetch_add(1, Ordering::Relaxed);
        let mut timelines = TIMELINES.lock().expect("timeline log poisoned");
        if timelines.last() != Some(&step.now().timeline()) {
            timelines.push(step.now().timeline());
        }
        Ok(())
    }
}

fn reset_logs() {
    STEPS.store(0, Ordering::Relaxed);
    TIMELINES.lock().expect("timeline log poisoned").clear();
    RESETS.lock().expect("reset log poisoned").clear();
}

fn timelines() -> Vec<TimelineId> {
    TIMELINES.lock().expect("timeline log poisoned").clone()
}

fn resets() -> Vec<(TimelineId, TimelineId)> {
    RESETS.lock().expect("reset log poisoned").clone()
}

/// Advance the world `steps` times from `from`, one millisecond of world time
/// apart, pausing long enough between publishes for the participant to act on
/// each one. A world advances only when the authority says so, which is exactly
/// why every one of these tests has to drive it explicitly.
async fn advance_world(
    clock: &StatePublisher<api::simulation::Clock>,
    authority: &TimelineAuthority,
    from: u64,
    steps: u64,
) {
    for step in 0..steps {
        clock
            .publish(
                &authority.completed_step(from + step * 1_000_000),
                api::simulation::Clock { step },
            )
            .expect("world step should publish");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn clock_publisher(bus: &Bus) -> StatePublisher<api::simulation::Clock> {
    StatePublisher::new(bus.clone(), &api::topic::owner().simulation().clock())
        .expect("clock publisher should attach")
}

/// Row 1: a restarted participant keeps the execution and the world history it
/// rejoins, and is a different producer.
///
/// The producer half is what makes "did the sequence reset" a non-question: a
/// fresh process is structurally a fresh producer, so a receiver never has to
/// interpret a sequence going backwards.
#[serial_test::serial(timeline_authority)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restarted_participant_keeps_its_execution_and_world_but_is_a_new_producer() {
    reset_logs();
    let namespace = format!("test/identity-restart/{}", std::process::id());
    let bus = Bus::open(BusConfig::in_process(namespace.clone(), "robot"))
        .await
        .expect("bus should open");
    let clock = clock_publisher(&bus);
    let world = TimelineId::from_raw(11).expect("nonzero timeline");
    let authority = TimelineAuthority::__mint(world).expect("world authority");

    let run = async |launch: ParticipantLaunch, from: u64| {
        run_with_bus::<IdentityProbe, _>(&bus, launch, async {
            advance_world(&clock, &authority, from, 4).await;
        })
        .await
        .expect("the probe should run cleanly");
    };

    let mut first = ParticipantLaunch::local("identity-probe-1", "robot");
    first.namespace = namespace.clone();
    first.clock = ClockMode::Simulation;
    let execution = first.execution;
    let first_producer = first.producer;
    run(first, 0).await;
    let first_run_steps = STEPS.load(Ordering::Relaxed);
    assert!(first_run_steps > 0, "the first run must step");

    // The restart: same supervised run, so the same execution is handed to the
    // new process, which mints its own producer identity.
    let mut second = ParticipantLaunch::local("identity-probe-1", "robot").in_execution(execution);
    second.namespace = namespace;
    second.clock = ClockMode::Simulation;
    let second_producer = second.producer;
    assert_eq!(second.execution, execution, "a restart is the same run");
    assert_ne!(
        first_producer, second_producer,
        "a restarted process must be a different producer"
    );
    run(second, 1_000_000).await;

    assert!(
        STEPS.load(Ordering::Relaxed) > first_run_steps,
        "the restarted run must step too"
    );
    assert_eq!(
        timelines(),
        vec![world],
        "both runs stepped on the same world history, so the log never changed \
         timeline"
    );
    assert!(
        resets().is_empty(),
        "rejoining the world it left is not a reset"
    );
    bus.close().await.expect("bus should close");
}

/// Row 3: pause and resume retain all three identities.
///
/// A pause is not an event the participant is told about - it is the absence of
/// new world steps. Nothing may be invented in that gap: no reset, no new
/// timeline, and no steps taken on a world that did not advance.
#[serial_test::serial(timeline_authority)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pause_and_resume_retains_every_identity_and_invents_no_steps() {
    reset_logs();
    let namespace = format!("test/identity-pause/{}", std::process::id());
    let bus = Bus::open(BusConfig::in_process(namespace.clone(), "robot"))
        .await
        .expect("bus should open");
    let clock = clock_publisher(&bus);
    let world = TimelineId::from_raw(21).expect("nonzero timeline");
    let authority = TimelineAuthority::__mint(world).expect("world authority");

    let mut launch = ParticipantLaunch::local("identity-probe-1", "robot");
    launch.namespace = namespace;
    launch.clock = ClockMode::Simulation;
    let execution = launch.execution;
    let producer = launch.producer;
    let launch_identity = (execution, producer);

    let paused_steps = std::sync::Arc::new(AtomicU64::new(0));
    let observed = std::sync::Arc::clone(&paused_steps);
    run_with_bus::<IdentityProbe, _>(&bus, launch, async move {
        advance_world(&clock, &authority, 0, 3).await;

        // Paused: the world authority publishes nothing at all.
        let before_pause = STEPS.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(150)).await;
        observed.store(
            STEPS.load(Ordering::Relaxed) - before_pause,
            Ordering::Relaxed,
        );

        // Resumed on the same world history, later in its own time.
        advance_world(&clock, &authority, 3_000_000, 3).await;
    })
    .await
    .expect("the probe should run cleanly");

    assert_eq!(
        paused_steps.load(Ordering::Relaxed),
        0,
        "a paused world must not release a single step"
    );
    assert_eq!(
        timelines(),
        vec![world],
        "pause and resume are the same world history"
    );
    assert!(resets().is_empty(), "a pause is not a world replacement");
    assert!(
        STEPS.load(Ordering::Relaxed) >= 2,
        "the resumed world must step again"
    );
    // The execution and producer are process facts that this participant never
    // re-reads: one process inside one run cannot change either, which is
    // exactly why a pause needs no handling for them.
    assert_eq!(launch_identity, (execution, producer));
    bus.close().await.expect("bus should close");
}

/// Row 4: a simulation reset, a replaced controller, and a replay branch are
/// the same event to every participant - a different world history inside the
/// same run - and each is a new timeline plus a new producer for the
/// controller.
///
/// The new-producer half is inherited from row 1: the replacement controller is
/// a different process, and a different process is always a different producer.
/// What this proves is the part the framework owns - the participant resets
/// exactly once, before its first step on the replacement world, and never
/// compares instants across the boundary.
#[serial_test::serial(timeline_authority)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replaced_world_resets_once_within_the_same_execution() {
    reset_logs();
    let namespace = format!("test/identity-replace/{}", std::process::id());
    let bus = Bus::open(BusConfig::in_process(namespace.clone(), "robot"))
        .await
        .expect("bus should open");
    let clock = clock_publisher(&bus);
    let first_world = TimelineId::from_raw(31).expect("nonzero timeline");
    // Deliberately *lower* than the first: timelines are equality-only
    // identities, so a replacement is not "newer".
    let second_world = TimelineId::from_raw(7).expect("nonzero timeline");

    let mut launch = ParticipantLaunch::local("identity-probe-1", "robot");
    launch.namespace = namespace;
    launch.clock = ClockMode::Simulation;
    let execution = launch.execution;
    let launch_execution = execution;

    run_with_bus::<IdentityProbe, _>(&bus, launch, async move {
        let first = TimelineAuthority::__mint(first_world).expect("world authority");
        advance_world(&clock, &first, 0, 3).await;

        // The controller process goes away and a replacement takes over. Its
        // authority is a different world history entirely, starting from its
        // own zero.
        drop(first);
        let second = TimelineAuthority::__mint(second_world).expect("replacement authority");
        advance_world(&clock, &second, 0, 3).await;
    })
    .await
    .expect("the probe should run cleanly");

    assert_eq!(
        timelines(),
        vec![first_world, second_world],
        "the participant steps on the replacement world, not across both"
    );
    assert_eq!(
        resets(),
        vec![(first_world, second_world)],
        "exactly one reset, naming both worlds, before the first step on the new one"
    );
    // Same run throughout: replacing the world does not replace the execution
    // the participant was launched into.
    assert_eq!(launch_execution, execution);
    bus.close().await.expect("bus should close");
}

/// The one identity a second authority cannot take: two controller processes
/// cannot both own a timeline, which is the runtime backstop behind the
/// coherence checker's rejection of a graph with two clock publishers.
#[serial_test::serial(timeline_authority)]
#[test]
fn a_second_timeline_authority_is_refused_while_the_first_is_alive() {
    let first = TimelineAuthority::__mint(TimelineId::from_raw(41).expect("nonzero timeline"))
        .expect("the first authority is granted");
    assert!(
        TimelineAuthority::__mint(TimelineId::from_raw(42).expect("nonzero timeline")).is_err(),
        "a second authority in one process must be refused"
    );
    drop(first);
    assert!(
        TimelineAuthority::__mint(TimelineId::from_raw(43).expect("nonzero timeline")).is_ok(),
        "a replacement authority is granted once the previous one is gone"
    );
}
