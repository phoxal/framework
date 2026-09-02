//! What a simulator session promises, proven over the real in-process
//! transport rather than against a recording of intent.
//!
//! Every test is `#[serial]`: a process holds exactly one timeline authority,
//! so two sessions may not overlap, and each test drops its own before the
//! next one opens.

use std::time::Duration;

use serial_test::serial;

use super::*;
use crate::api;
use crate::bus::{
    CaptureStamp, ParticipantReadyStatus, RobotInstant, SampleReceiver, SetpointPublisher,
    StepStamp, StreamReceiver,
};
use crate::identity::ComponentInstanceId;
use crate::model::identity::CapabilityId;

const LABEL: &str = "simulator-test";
const RECEIVE: Duration = Duration::from_secs(2);

fn component() -> ComponentInstanceId {
    ComponentInstanceId::new("left_drive").expect("a valid component instance")
}

fn capability(id: &str) -> CapabilityId {
    CapabilityId::new(id).expect("a valid capability id")
}

fn driver() -> ParticipantId {
    ParticipantId::new("left_drive").expect("a valid participant id")
}

/// The world-step contract, end to end: one advance mints one token, every
/// output of that advance carries it, and the clock that closes the step
/// enqueues after all of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn a_step_publishes_its_outputs_before_the_clock_that_closes_it() {
    let mut session = SimulatorSession::in_process(LABEL)
        .await
        .expect("the in-process simulator session opens");
    let encoder = session
        .sample_publisher(
            api::topics()
                .component(&component())
                .expect("a concrete component segment")
                .encoder(&capability("encoder"))
                .expect("a concrete capability segment")
                .sample()
                .owner(),
        )
        .expect("the encoder publisher attaches");
    let samples = SampleReceiver::<api::component::encoder::Sample>::new(
        &session.bus,
        &api::topics()
            .component(&component())
            .expect("a concrete component segment")
            .encoder(&capability("encoder"))
            .expect("a concrete capability segment")
            .sample()
            .client(),
    )
    .await
    .expect("the encoder subscriber attaches");
    let clocks = StreamReceiver::<Clock>::new(
        &session.bus,
        &crate::simulation::api::topics().clock().client(),
    )
    .await
    .expect("the clock subscriber attaches");

    let mut world = session.take_world_time().expect("world time is available");
    let timeline = world.timeline();

    let step = world.completed_step(20_000_000);
    encoder
        .publish(
            CaptureStamp::exact(step.instant()),
            api::component::encoder::Sample::try_new(1.0, 0.5).expect("a finite sample"),
        )
        .expect("the sample is admitted");
    world
        .publish_clock(&step, Clock { step: 1 })
        .expect("the clock is admitted");

    let sample = tokio::time::timeout(RECEIVE, samples.recv())
        .await
        .expect("the encoder sample arrives")
        .expect("the encoder sample decodes");
    let tick = tokio::time::timeout(RECEIVE, clocks.recv())
        .await
        .expect("the clock arrives")
        .expect("the clock decodes");

    let expected = RobotInstant::new(timeline, 20_000_000);
    assert_eq!(sample.metadata.produced_exactly_at(), Some(expected));
    assert_eq!(tick.metadata.produced_exactly_at(), Some(expected));
    assert_eq!(tick.body.step, 1);
    assert!(
        sample.metadata.sequence < tick.metadata.sequence,
        "every output of a completed step must enqueue before the clock that closes it"
    );

    drop(world);
    session
        .close()
        .await
        .expect("the simulator session closes cleanly");
}

/// The graph's setpoints reach the capability this simulator owns, decoded as
/// the endpoint's own body.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn typed_component_setpoints_reach_the_capability_the_simulator_owns() {
    let session = SimulatorSession::in_process(LABEL)
        .await
        .expect("the in-process simulator session opens");
    let commands = session
        .setpoint_receiver(
            api::topics()
                .component(&component())
                .expect("a concrete component segment")
                .motor(&capability("motor"))
                .expect("a concrete capability segment")
                .command()
                .owner(),
        )
        .await
        .expect("the motor receiver attaches");
    let drive = SetpointPublisher::<api::component::motor::Command>::new(
        session.bus.clone(),
        &api::topics()
            .component(&component())
            .expect("a concrete component segment")
            .motor(&capability("motor"))
            .expect("a concrete capability segment")
            .command()
            .client(),
    )
    .expect("the drive publisher attaches");

    drive
        .send(api::component::motor::Command::Velocity(0.25))
        .expect("the command is admitted");

    let observed = tokio::time::timeout(RECEIVE, async {
        loop {
            if let Some(observed) = commands.try_recv() {
                return observed;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the command arrives");
    assert_eq!(
        observed.body,
        api::component::motor::Command::Velocity(0.25)
    );

    session
        .close()
        .await
        .expect("the simulator session closes cleanly");
}

/// Delegated presence is what makes a simulated robot read as exactly as
/// present as the same robot on hardware: the lease appears under the driver's
/// own participant id, and it goes when the session does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn delegated_presence_appears_under_the_driver_identity_and_leaves_with_the_session() {
    let mut session = SimulatorSession::in_process(LABEL)
        .await
        .expect("the in-process simulator session opens");
    let events = session
        .participant_ready_events(&driver())
        .await
        .expect("the ready observer attaches");

    session.present(&driver()).await.expect("presence declared");
    session
        .present(&driver())
        .await
        .expect("a repeated presence is a no-op");

    let ready = next_status(&events).await;
    assert_eq!(ready.0, driver());
    assert_eq!(ready.1, ParticipantReadyStatus::Ready);

    session
        .close()
        .await
        .expect("the simulator session closes cleanly");

    let lost = next_status(&events).await;
    assert_eq!(lost.0, driver());
    assert_eq!(lost.1, ParticipantReadyStatus::Lost);
}

/// A rewind is a new world history, not a jump backwards inside the old one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn replacing_the_timeline_starts_a_new_world_history() {
    let mut session = SimulatorSession::in_process(LABEL)
        .await
        .expect("the in-process simulator session opens");
    let mut world = session.take_world_time().expect("world time is available");

    let before = world.timeline();
    let first = world.completed_step(20_000_000);
    world.replace_timeline();
    let after = world.timeline();
    let second = world.completed_step(20_000_000);

    assert_ne!(before, after, "a rewind mints a new timeline");
    assert_ne!(
        first.instant(),
        second.instant(),
        "the same tick on a new timeline is a different instant"
    );
    assert_eq!(second.instant(), RobotInstant::new(after, 20_000_000));

    drop(world);
    session
        .close()
        .await
        .expect("the simulator session closes cleanly");
}

/// A world has one hand. Taking it twice is a failure, not a second one, and
/// the session that has handed it out still closes deterministically.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn world_time_is_taken_exactly_once_and_close_stays_deterministic() {
    let mut session = SimulatorSession::in_process(LABEL)
        .await
        .expect("the in-process simulator session opens");
    let world = session.take_world_time().expect("world time is available");
    assert!(matches!(
        session.take_world_time(),
        Err(SimulatorError::WorldTimeTaken)
    ));

    drop(world);
    session
        .close()
        .await
        .expect("the simulator session closes cleanly");

    // The authority is a per-process singleton, so a session that opens after
    // a clean close proves the previous one released everything it held.
    let next = SimulatorSession::in_process(LABEL)
        .await
        .expect("a closed session releases the world authority");
    next.close()
        .await
        .expect("the second session closes cleanly");
}

/// The next Ready change for the observed participant.
async fn next_status(
    events: &ParticipantReadyEvents,
) -> (ParticipantId, crate::bus::ParticipantReadyStatus) {
    tokio::time::timeout(RECEIVE, async {
        loop {
            if let Some(event) = events.try_recv() {
                return (event.participant().clone(), event.status);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a ready change arrives")
}
