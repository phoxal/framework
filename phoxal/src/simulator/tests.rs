use super::*;
use crate::bus::DeliveryFamily;
use crate::identity::TimelineId;
use crate::model::identity::CapabilityId;
use crate::model::world::WorldProgress;

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_identity_loss_ends_observation_even_when_streams_stay_open() {
    for controller in [true, false] {
        let (owner, bus) = BusOwner::open(BusConfig::for_external(
            ExecutionId::mint(),
            None,
            Vec::new(),
        ))
        .await
        .unwrap();
        let identity = owner
            .declare_liveliness_key(crate::supervisor::api::connect::PRESENCE_KEY)
            .await
            .unwrap();
        let attachments = crate::bus::StreamReceiver::new(
            &bus,
            &crate::supervisor::api::topics()
                .simulation()
                .attachment()
                .client(),
        )
        .await
        .unwrap();
        let domains = crate::bus::StreamReceiver::new(
            &bus,
            &crate::supervisor::api::topics().time_domain().client(),
        )
        .await
        .unwrap();
        let (attachment, _current) = tokio::sync::watch::channel(None);
        let transitions = (!controller).then(|| tokio::sync::broadcast::channel(8).0);
        let fault = Arc::new(Mutex::new(None));
        bus.set_active_simulation_binding(Some((bus.producer(), 7)));
        let observer = tokio::spawn(observe_attachment(
            attachment,
            transitions,
            attachments,
            domains,
            TimeDomain {
                revision: 1,
                timeline: TimelineId::mint(),
                mode: TimeMode::Monotonic,
            },
            Arc::clone(&fault),
            bus.clone(),
        ));
        tokio::task::yield_now().await;
        drop(identity);
        tokio::time::timeout(std::time::Duration::from_secs(5), observer)
            .await
            .expect("identity loss cannot wait for a retained stream")
            .unwrap();
        assert!(
            fault
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .contains("supervisor identity")
        );
        if controller {
            assert!(
                bus.active_simulation_delivery_metadata(
                    bus.producer(),
                    7,
                    DeliveryFamily::Sample,
                    None,
                )
                .unwrap()
                .is_none()
            );
        }
        let _ = owner.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn live_publishers_fail_closed_outside_the_exact_active_binding() {
    let (owner, bus) = BusOwner::open(BusConfig::for_external(
        ExecutionId::mint(),
        None,
        Vec::new(),
    ))
    .await
    .expect("test simulator bus opens");
    let progress = WorldProgress::at(1, 12).expect("valid progress");
    let transition = LiveTransitionStamp {
        instant: RobotInstant::new(TimelineId::mint(), 1),
        world: WorldInstanceId::mint(),
        revision: 7,
        attached_at: LiveAttachmentBoundary {
            world: WorldProgress::zero(12).expect("valid initial progress"),
            execution: RobotInstant::new(TimelineId::mint(), 0),
        },
        progress,
    };

    assert!(
        bus.active_simulation_delivery_metadata(
            bus.producer(),
            transition.revision,
            crate::bus::DeliveryFamily::Sample,
            Some(crate::bus::TimeWindow::exact(transition.instant())),
        )
        .expect("metadata check succeeds")
        .is_none()
    );
    bus.set_active_simulation_binding(Some((bus.producer(), 6)));
    assert!(
        bus.active_simulation_delivery_metadata(
            bus.producer(),
            transition.revision,
            crate::bus::DeliveryFamily::Sample,
            Some(crate::bus::TimeWindow::exact(transition.instant())),
        )
        .expect("metadata check succeeds")
        .is_none()
    );
    bus.set_active_simulation_binding(Some((bus.producer(), 7)));
    let metadata = bus
        .active_simulation_delivery_metadata(
            bus.producer(),
            transition.revision,
            crate::bus::DeliveryFamily::Sample,
            Some(crate::bus::TimeWindow::exact(transition.instant())),
        )
        .expect("metadata check succeeds")
        .expect("the exact current Active binding admits publication");
    assert_eq!(metadata.attachment_revision, Some(transition.revision));
    bus.set_active_simulation_binding(None);
    assert!(
        bus.active_simulation_delivery_metadata(
            bus.producer(),
            transition.revision,
            crate::bus::DeliveryFamily::Sample,
            Some(crate::bus::TimeWindow::exact(transition.instant())),
        )
        .expect("metadata check succeeds")
        .is_none()
    );

    let _ = owner.close().await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_live_transition_admits_outputs_then_step_with_one_exact_instant() {
    let execution = ExecutionId::mint();
    let (owner, bus) = BusOwner::open(BusConfig::for_external(execution, None, Vec::new()))
        .await
        .expect("test simulator bus opens");
    let state = LiveStatePublisher {
        inner: StatePublisher::new(bus.clone(), &crate::api::topics().drive().state().owner())
            .expect("state publisher"),
        bus: bus.clone(),
    };
    let component =
        crate::identity::ComponentInstanceId::new("accelerometer").expect("component id");
    let capability = CapabilityId::new("linear").expect("capability id");
    let sample_topic = crate::api::topics()
        .component(&component)
        .expect("component topic")
        .accelerometer(&capability)
        .expect("accelerometer topic")
        .sample()
        .owner();
    let sample = LiveSamplePublisher {
        inner: SamplePublisher::new(bus.clone(), &sample_topic).expect("sample publisher"),
        bus: bus.clone(),
    };
    let step = EventPublisher::new(
        bus.clone(),
        &crate::simulation::api::topics().step().owner(),
    )
    .expect("step publisher");
    let pause = bus
        .test_pause_outbound_drain()
        .await
        .expect("the one bus drain can be held before admission");
    let timeline = TimelineId::mint();
    let revision = 7;
    let transition = LiveTransitionStamp {
        instant: RobotInstant::new(timeline, 123),
        world: WorldInstanceId::mint(),
        revision,
        attached_at: LiveAttachmentBoundary {
            world: WorldProgress::zero(12).expect("initial world progress"),
            execution: RobotInstant::new(timeline, 100),
        },
        progress: WorldProgress::at(1, 12).expect("completed world transition"),
    };
    bus.set_active_simulation_binding(Some((bus.producer(), revision)));

    state
        .publish(
            &transition,
            crate::api::drive::State::Stopped {
                target: crate::api::drive::Target::stopped(),
                reason: crate::api::drive::StopReason::Fault,
            },
        )
        .expect("state output is admitted without draining");
    sample
        .publish(
            &transition,
            crate::api::component::accelerometer::Sample::try_new([1.0, 2.0, 3.0])
                .expect("finite sample"),
        )
        .expect("sample output is admitted without draining");
    admit_step_event(
        &step,
        &bus,
        &transition,
        StepEvent {
            index: transition.progress().completed_step(),
        },
    )
    .expect("StepEvent is admitted without draining");

    let mut queued = bus.test_queued_delivery_metadata();
    queued.sort_by_key(|(_, _, metadata)| metadata.sequence);
    assert_eq!(
        queued.len(),
        3,
        "all publications use the one bus scheduler"
    );
    assert_eq!(
        queued
            .iter()
            .map(|(_, family, _)| *family)
            .collect::<Vec<_>>(),
        vec![
            DeliveryFamily::State,
            DeliveryFamily::Sample,
            DeliveryFamily::Stream,
        ]
    );
    assert!(queued[0].0.ends_with("robot/drive/state"));
    assert!(
        queued[1]
            .0
            .ends_with("robot/component/accelerometer/accelerometer/linear/sample")
    );
    assert!(queued[2].0.ends_with("simulation/step"));
    assert_eq!(
        queued
            .iter()
            .map(|(_, _, metadata)| metadata.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "the StepEvent is admitted after every output in local producer order"
    );
    for (_, _, metadata) in &queued {
        assert_eq!(metadata.produced_exactly_at(), Some(transition.instant()));
        assert_eq!(metadata.attachment_revision, Some(revision));
    }

    drop(pause);
    let _ = owner
        .close_until(tokio::time::Instant::now() + std::time::Duration::from_secs(10))
        .await;
}

#[test]
fn a_transition_stamp_keeps_execution_and_world_time_separate() {
    let timeline = TimelineId::mint();
    let world = WorldInstanceId::mint();
    let attached_at = LiveAttachmentBoundary {
        world: WorldProgress::at(4, 12).unwrap(),
        execution: RobotInstant::new(timeline, 90),
    };
    let stamp = LiveTransitionStamp {
        instant: RobotInstant::new(timeline, 100),
        world,
        revision: 7,
        attached_at,
        progress: WorldProgress::at(5, 12).unwrap(),
    };
    assert_eq!(stamp.instant(), RobotInstant::new(timeline, 100));
    assert_eq!(stamp.world(), world);
    assert_eq!(stamp.revision(), 7);
    assert_eq!(stamp.attached_at(), attached_at);
    assert_eq!(stamp.progress().completed_step(), 5);
}

#[test]
fn an_active_boundary_carries_no_world_progress_or_step_authority() {
    let timeline = TimelineId::mint();
    let world = WorldInstanceId::mint();
    let local = LocalInstant::from_boot_ns(100);
    let attached_at = LiveAttachmentBoundary {
        world: WorldProgress::at(4, 12).unwrap(),
        execution: RobotInstant::new(timeline, 90),
    };
    let boundary = ActiveBoundaryStamp {
        local,
        instant: RobotInstant::new(timeline, 100),
        world,
        revision: 7,
        attached_at,
    };
    assert_eq!(boundary.local_instant(), local);
    assert_eq!(boundary.instant(), RobotInstant::new(timeline, 100));
    assert_eq!(boundary.world(), world);
    assert_eq!(boundary.revision(), 7);
    assert_eq!(boundary.attached_at(), attached_at);
}

#[test]
fn transition_progress_must_advance_by_one_exact_quantum() {
    let previous = WorldProgress::at(4, 12).expect("valid progress");
    validate_next_progress(
        previous,
        WorldProgress::at(5, 12).expect("the next exact quantum"),
    )
    .expect("one exact transition is accepted");
    assert!(matches!(
        validate_next_progress(
            previous,
            WorldProgress::at(6, 12).expect("valid but skipped progress")
        ),
        Err(SimulatorError::NonMonotonicProgress {
            previous: 4,
            observed: 6,
        })
    ));
    let inconsistent: WorldProgress = serde_json::from_value(serde_json::json!({
        "completed_step": 5,
        "elapsed_ns": 65,
    }))
    .expect("the fields imply a positive quantum before session validation");
    assert!(matches!(
        validate_next_progress(previous, inconsistent),
        Err(SimulatorError::InvalidProgress(
            crate::model::world::WorldProgressError::Inconsistent { .. }
        ))
    ));
}
