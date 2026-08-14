//! Deterministic tests of the supervisor-owned endpoint decisions.
//!
//! A real client attachment crosses process, socket, and Zenoh boundaries and
//! is therefore host acceptance evidence, not a repository test. These tests
//! exercise the decisions immediately behind those endpoints without creating
//! another protocol model or opening a transport.

use std::fmt::Debug;
use std::fs;

use phoxal_bus::{Codec, EndpointDescriptor, EndpointKind, MessagePack, QueryEndpointDescriptor};
use phoxal_model::builder::{Kinematics, RobotBuilder};
use phoxal_model::robot::MotionLimits;
use phoxal_protocol::supervisor::connect::{ConnectReply, ConnectRequest};
use phoxal_protocol::supervisor::execution::{Command, CommandOutcome, CommandRejection};
use phoxal_protocol::{runtime, supervisor};
use phoxal_runtime_contract::identity::{ParticipantId, ProducerId};
use phoxal_runtime_contract::version::FrameworkVersion;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{Control, HostAction, PostReply, bundle_entry, command, connect_reply, manual_drive};
use crate::process::spec::SupervisorAction;
use crate::state::store::SupervisorState;
use crate::supervisor::projection::ExecutionFacts;
use crate::supervisor::roster::Roster;
use crate::supervisor::state::ExecutionState;

#[test]
fn connect_reply_reports_the_owner_framework_train() {
    assert_eq!(
        connect_reply(),
        ConnectReply::V0 {
            framework: FrameworkVersion::CURRENT,
        }
    );
}

#[test]
fn generated_endpoints_pin_the_supervisor_boundary() {
    assert_query_round_trip::<supervisor::endpoint::connect::TopicEndpoint>(
        "supervisor/connect",
        ConnectRequest::V0 {},
        connect_reply(),
    );

    let (state, _, _) = ready_state();
    let snapshot = supervisor::execution::SnapshotDocument::V0(state.snapshot());
    assert_stream_round_trip::<supervisor::endpoint::snapshot::TopicEndpoint>(
        "supervisor/snapshot",
        snapshot.clone(),
    );
    assert_query_round_trip::<supervisor::endpoint::snapshot::CurrentEndpoint>(
        "supervisor/snapshot/current",
        supervisor::snapshot::CurrentRequest {},
        snapshot,
    );

    assert_query_round_trip::<supervisor::endpoint::info::TopicEndpoint>(
        "supervisor/info",
        supervisor::info::InfoRequest {},
        supervisor::info::Info {
            robot: phoxal_runtime_contract::identity::RobotId::new("rover").expect("fixture robot"),
            clock: phoxal_runtime_contract::clock::Clock::Real,
            manual_drive: Some(supervisor::info::ManualDrive {
                wheel_base_m: 0.42,
                max_linear_speed_mps: 0.8,
                max_angular_speed_radps: 1.6,
            }),
        },
    );

    assert_query_round_trip::<supervisor::endpoint::logs::SnapshotEndpoint>(
        "supervisor/logs/snapshot",
        supervisor::logs::SnapshotRequest {
            participant_id: Some("brain".to_owned()),
            limit: 7,
            before_sequence: Some(11),
        },
        supervisor::logs::Snapshot {
            cursor: runtime::telemetry::Cursor { sequence: 13 },
            ingest_dropped: 2,
            records: Vec::new(),
            next_before_sequence: Some(5),
        },
    );
    assert_endpoint::<supervisor::endpoint::logs::FollowEndpoint>(
        "supervisor/logs/follow",
        EndpointKind::Stream,
    );

    assert_query_round_trip::<supervisor::endpoint::telemetry::SnapshotEndpoint>(
        "supervisor/telemetry/snapshot",
        supervisor::telemetry::SnapshotRequest {
            participant_id: None,
            limit: 9,
            before_sequence: Some(17),
        },
        supervisor::telemetry::Snapshot {
            cursor: runtime::telemetry::Cursor { sequence: 19 },
            records: Vec::new(),
            capacity_evictions: 3,
            next_before_sequence: None,
        },
    );
    assert_endpoint::<supervisor::endpoint::telemetry::FollowEndpoint>(
        "supervisor/telemetry/follow",
        EndpointKind::Stream,
    );

    assert_query_round_trip::<supervisor::endpoint::command::TopicEndpoint>(
        "supervisor/command",
        supervisor::command::Request::V0 {
            command: Command::Stop {
                expected_revision: 23,
            },
        },
        supervisor::command::Reply::V0 {
            outcome: CommandOutcome::Accepted { at_revision: 23 },
        },
    );
    assert_query_round_trip::<supervisor::endpoint::bundle::GetEndpoint>(
        "supervisor/bundle/get",
        supervisor::bundle::GetRequest {
            path: "assets/map.bin".to_owned(),
        },
        supervisor::bundle::GetResponse::Found {
            bytes: vec![1, 2, 3],
        },
    );
}

#[test]
fn info_projects_manual_drive_only_for_differential_robots() {
    let limits = MotionLimits {
        max_linear_speed_mps: 0.8,
        max_angular_speed_radps: 1.6,
    };
    let differential = RobotBuilder::new("rover")
        .component_type("motor", |motor| motor.motor("spin", "axle"))
        .component("left", "motor")
        .component("right", "motor")
        .kinematics(Kinematics::Differential {
            left_actuators: &["left.spin"],
            right_actuators: &["right.spin"],
            left_encoders: &[],
            right_encoders: &[],
            wheel_radius_m: 0.1,
            wheel_base_m: 0.42,
        })
        .motion_limits(limits)
        .build()
        .expect("valid differential robot");
    assert_eq!(
        manual_drive(&differential),
        Some(supervisor::info::ManualDrive {
            wheel_base_m: 0.42,
            max_linear_speed_mps: 0.8,
            max_angular_speed_radps: 1.6,
        })
    );

    let omnidirectional = RobotBuilder::new("platform")
        .motion_limits(limits)
        .build()
        .expect("valid non-differential robot");
    assert_eq!(manual_drive(&omnidirectional), None);
}

#[test]
fn bundle_entry_serves_only_plain_relative_files() {
    let root = tempfile::tempdir().expect("temporary bundle root");
    fs::write(root.path().join("runtime.json"), b"runtime").expect("runtime fixture");
    fs::create_dir(root.path().join("assets")).expect("asset directory");
    fs::write(root.path().join("assets/map.bin"), b"map").expect("asset fixture");

    assert_eq!(
        bundle_entry(root.path(), "runtime.json"),
        supervisor::bundle::GetResponse::Found {
            bytes: b"runtime".to_vec(),
        }
    );
    assert_eq!(
        bundle_entry(root.path(), "assets/map.bin"),
        supervisor::bundle::GetResponse::Found {
            bytes: b"map".to_vec(),
        }
    );
    assert_eq!(
        bundle_entry(root.path(), "assets/missing.bin"),
        supervisor::bundle::GetResponse::Missing
    );
    for refused in ["", "../outside", "/etc/passwd", "assets/../runtime.json"] {
        assert_eq!(
            bundle_entry(root.path(), refused),
            supervisor::bundle::GetResponse::InvalidPath,
            "{refused:?}"
        );
    }
}

#[tokio::test]
async fn stop_is_applied_only_after_the_reply_decision() {
    let (state, _, _) = ready_state();
    let (control, _receiver) = test_control(1);
    let revision = state.snapshot().revision;

    let (outcome, post_reply) = command(
        &state,
        &control,
        Command::Stop {
            expected_revision: revision,
        },
    );
    assert_eq!(
        outcome,
        CommandOutcome::Accepted {
            at_revision: revision,
        }
    );
    assert_eq!(post_reply, PostReply::Stop);
    assert!(!control.stop.is_cancelled(), "acceptance precedes shutdown");
    post_reply.apply(&control).await;
    assert!(control.stop.is_cancelled(), "shutdown follows the reply");

    let (control, _receiver) = test_control(1);
    let (outcome, post_reply) = command(
        &state,
        &control,
        Command::Stop {
            expected_revision: revision.saturating_add(1),
        },
    );
    assert_rejected(outcome, CommandRejection::RevisionStale);
    assert_eq!(post_reply, PostReply::None);
    assert!(!control.stop.is_cancelled());
}

#[test]
fn machine_actions_are_revision_guarded_and_scheduled_after_the_reply() {
    let (state, _, _) = ready_state();
    let revision = state.snapshot().revision;
    for (request, action) in [
        (
            Command::Reboot {
                expected_revision: revision,
            },
            HostAction::Reboot,
        ),
        (
            Command::Poweroff {
                expected_revision: revision,
            },
            HostAction::Poweroff,
        ),
    ] {
        let (control, _receiver) = test_control(1);
        let (outcome, post_reply) = command(&state, &control, request);
        assert_eq!(
            outcome,
            CommandOutcome::Accepted {
                at_revision: revision
            }
        );
        assert_eq!(post_reply, PostReply::Host(action));

        let stale = match action {
            HostAction::Reboot => Command::Reboot {
                expected_revision: revision + 1,
            },
            HostAction::Poweroff => Command::Poweroff {
                expected_revision: revision + 1,
            },
        };
        let (outcome, post_reply) = command(&state, &control, stale);
        assert_rejected(outcome, CommandRejection::RevisionStale);
        assert_eq!(post_reply, PostReply::None);
    }
}

#[test]
fn restart_rejects_an_unknown_participant_and_a_stale_producer() {
    let (state, participant, producer) = ready_state();
    let (control, _receiver) = test_control(1);

    let (outcome, post_reply) = command(
        &state,
        &control,
        Command::Restart {
            participant: ParticipantId::new("unknown").expect("unknown fixture participant"),
            expected_producer: None,
        },
    );
    assert_rejected(outcome, CommandRejection::UnknownParticipant);
    assert_eq!(post_reply, PostReply::None);

    let stale = ProducerId::try_from((1_u128 << 124) + 2).expect("stale fixture producer");
    assert_ne!(stale, producer);
    let (outcome, post_reply) = command(
        &state,
        &control,
        Command::Restart {
            participant,
            expected_producer: Some(stale),
        },
    );
    assert_rejected(outcome, CommandRejection::ProducerFenced);
    assert_eq!(post_reply, PostReply::None);
}

#[test]
fn restart_accepts_the_current_producer_and_queues_the_exact_process() {
    let (state, participant, producer) = ready_state();
    let expected_key = participant.clone().into();
    let (control, mut receiver) = test_control(1);
    let revision = state.snapshot().revision;

    let (outcome, post_reply) = command(
        &state,
        &control,
        Command::Restart {
            participant,
            expected_producer: Some(producer),
        },
    );
    assert_eq!(
        outcome,
        CommandOutcome::Accepted {
            at_revision: revision,
        }
    );
    assert_eq!(post_reply, PostReply::None);
    let queued = receiver.try_recv().expect("restart action is queued");
    let SupervisorAction::Restart { key } = queued;
    assert_eq!(key, expected_key);
}

#[test]
fn restart_reports_busy_and_closed_control_channels() {
    let (state, participant, producer) = ready_state();
    let key = participant.clone().into();
    let (busy, _receiver) = test_control(1);
    busy.actions
        .try_send(SupervisorAction::Restart { key })
        .expect("fill the bounded action channel");
    let (outcome, post_reply) = command(
        &state,
        &busy,
        Command::Restart {
            participant: participant.clone(),
            expected_producer: Some(producer),
        },
    );
    assert_rejected(outcome, CommandRejection::Busy);
    assert_eq!(post_reply, PostReply::None);

    let (closed, receiver) = test_control(1);
    drop(receiver);
    let (outcome, post_reply) = command(
        &state,
        &closed,
        Command::Restart {
            participant,
            expected_producer: Some(producer),
        },
    );
    assert_rejected(outcome, CommandRejection::ControlClosed);
    assert_eq!(post_reply, PostReply::None);
}

fn ready_state() -> (ExecutionState, ParticipantId, ProducerId) {
    let participant = ParticipantId::new("brain").expect("fixture participant");
    let producer = ProducerId::try_from(1_u128 << 124).expect("fixture producer");
    let board = SupervisorState::new();
    let state = ExecutionState::new(
        board.clone(),
        ExecutionFacts {
            roster: Roster::test_fixture(),
        },
    );
    board.register_planned(&participant.clone().into());
    assert_eq!(
        board.record_instance_presence(participant.clone(), producer, true),
        None
    );
    (state, participant, producer)
}

fn test_control(capacity: usize) -> (Control, mpsc::Receiver<SupervisorAction>) {
    let (actions, receiver) = mpsc::channel(capacity);
    (
        Control {
            actions,
            stop: CancellationToken::new(),
        },
        receiver,
    )
}

fn assert_rejected(outcome: CommandOutcome, reason: CommandRejection) {
    assert_eq!(outcome, CommandOutcome::Rejected { reason });
}

fn assert_endpoint<E: EndpointDescriptor>(topic: &str, kind: EndpointKind) {
    assert_eq!(E::TOPIC, topic);
    assert_eq!(E::KIND, kind);
}

fn assert_stream_round_trip<E>(topic: &str, payload: E::Payload)
where
    E: EndpointDescriptor,
    E::Payload: Debug + PartialEq,
{
    assert_endpoint::<E>(topic, EndpointKind::Stream);
    let encoded = MessagePack::encode(&payload).expect("endpoint payload encodes");
    let decoded = MessagePack::decode::<E::Payload>(&encoded).expect("endpoint payload decodes");
    assert_eq!(decoded, payload);
}

fn assert_query_round_trip<E>(topic: &str, request: E::Request, response: E::Response)
where
    E: QueryEndpointDescriptor,
    E::Request: Debug + PartialEq,
    E::Response: Debug + PartialEq,
{
    assert_endpoint::<E>(topic, EndpointKind::Query);
    let encoded = MessagePack::encode(&request).expect("endpoint request encodes");
    let decoded = MessagePack::decode::<E::Request>(&encoded).expect("endpoint request decodes");
    assert_eq!(decoded, request);

    let encoded = MessagePack::encode(&response).expect("endpoint response encodes");
    let decoded = MessagePack::decode::<E::Response>(&encoded).expect("endpoint response decodes");
    assert_eq!(decoded, response);
}
