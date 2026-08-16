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
use phoxal_protocol::supervisor::command::{Command, CommandOutcome, CommandRejection};
use phoxal_protocol::supervisor::connect::{ConnectReply, ConnectRequest};
use phoxal_protocol::{runtime, supervisor};
use phoxal_runtime_contract::identity::{ParticipantId, ProducerId};
use phoxal_runtime_contract::version::FrameworkVersion;

use super::{HostAction, bundle_entry, command, connect_reply, manual_drive};
use crate::supervisor::presence::Presence;
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

    let state = present_state().0;
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
            command: Command::Reboot {
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
    fs::write(root.path().join("manifest.json"), b"manifest").expect("manifest fixture");
    fs::create_dir(root.path().join("assets")).expect("asset directory");
    fs::write(root.path().join("assets/map.bin"), b"map").expect("asset fixture");

    assert_eq!(
        bundle_entry(root.path(), "manifest.json"),
        supervisor::bundle::GetResponse::Found {
            bytes: b"manifest".to_vec(),
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
    for refused in ["", "../outside", "/etc/passwd", "assets/../manifest.json"] {
        assert_eq!(
            bundle_entry(root.path(), refused),
            supervisor::bundle::GetResponse::InvalidPath,
            "{refused:?}"
        );
    }
}

/// Both host actions are guarded by the revision the operator was looking at,
/// and neither is performed before the acceptance has been decided.
#[test]
fn host_actions_are_revision_guarded_and_scheduled_after_the_reply() {
    let (state, _) = present_state();
    let revision = state.snapshot().revision;
    for (request, expected) in [
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
        let (outcome, action) = command(&state, request);
        assert_eq!(
            outcome,
            CommandOutcome::Accepted {
                at_revision: revision
            }
        );
        assert_eq!(action, Some(expected));

        let stale = match expected {
            HostAction::Reboot => Command::Reboot {
                expected_revision: revision + 1,
            },
            HostAction::Poweroff => Command::Poweroff {
                expected_revision: revision + 1,
            },
        };
        let (outcome, action) = command(&state, stale);
        assert_eq!(
            outcome,
            CommandOutcome::Rejected {
                reason: CommandRejection::RevisionStale,
            }
        );
        assert_eq!(action, None);
    }
}

/// One expected runtime, present under a known producer.
fn present_state() -> (ExecutionState, ParticipantId) {
    let robot = RobotBuilder::new("rover").build().expect("fixture robot");
    let state = ExecutionState::new(Presence::for_robot(&robot).expect("an expected set"));
    let participant = ParticipantId::new("brain").expect("fixture participant");
    state.record_presence(
        &participant,
        ProducerId::try_from(1_u128 << 124).expect("fixture producer"),
        true,
    );
    (state, participant)
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
