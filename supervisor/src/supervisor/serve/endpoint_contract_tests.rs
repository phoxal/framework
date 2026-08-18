//! Deterministic tests of the supervisor-owned endpoint decisions.
//!
//! A real client attachment crosses process, socket, and Zenoh boundaries and
//! is therefore host acceptance evidence, not a repository test. These tests
//! exercise the decisions immediately behind those endpoints without creating
//! another protocol model or opening a transport.

use std::fmt::Debug;
use std::fs;

use phoxal::bus::{
    Codec, Endpoint, EndpointKind, EndpointSemantics, MessagePack, Publish, QueryEndpoint,
    ServeQuery, Topic,
};
use phoxal::identity::{ParticipantId, ProducerId};
use phoxal::model::builder::RobotBuilder;
use phoxal::model::manifest::ManifestDocument;
use phoxal::runtime::api as runtime;
use phoxal::supervisor::api as supervisor;
use phoxal::supervisor::api::command::{Command, CommandOutcome};
use phoxal::supervisor::api::connect::{ConnectReply, ConnectRequest};
use phoxal::version::FrameworkVersion;

use super::{HostAction, bundle_entry, command, connect_reply};
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

/// The concrete keys the tree renders for the supervisor's own endpoints, and
/// the bodies each of them carries. Every key is read off the owner-side topic
/// this process actually binds, so a path or a leaf that moved shows up here as
/// the key it now renders.
#[test]
fn the_supervisor_boundary_is_pinned_to_its_rendered_keys() {
    let api = supervisor::topics();

    assert_query_round_trip(
        &api.connect().owner(),
        "supervisor/connect",
        ConnectRequest::V0 {},
        connect_reply(),
    );

    assert_query_endpoint(&api.info().owner(), "supervisor/info");
    let request =
        MessagePack::encode(&supervisor::info::InfoRequest {}).expect("a request encodes");
    MessagePack::decode::<supervisor::info::InfoRequest>(&request).expect("a request decodes");

    let state = present_state().0;
    let snapshot = supervisor::execution::SnapshotDocument::V0(state.snapshot());
    assert_stream_round_trip(
        &api.snapshot().owner(),
        "supervisor/snapshot",
        snapshot.clone(),
    );
    assert_query_round_trip(
        &api.snapshot().current().owner(),
        "supervisor/snapshot/current",
        supervisor::snapshot::CurrentRequest {},
        snapshot,
    );

    assert_query_round_trip(
        &api.logs().snapshot().owner(),
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
    assert_stream_endpoint(&api.logs().follow().owner(), "supervisor/logs/follow");

    assert_query_round_trip(
        &api.telemetry().snapshot().owner(),
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
    assert_stream_endpoint(
        &api.telemetry().follow().owner(),
        "supervisor/telemetry/follow",
    );

    assert_query_round_trip(
        &api.command().owner(),
        "supervisor/command",
        supervisor::command::Request::V0 {
            command: Command::Reboot,
        },
        supervisor::command::Reply::V0 {
            outcome: CommandOutcome::Accepted { at_revision: 23 },
        },
    );
    assert_query_round_trip(
        &api.bundle().get().owner(),
        "supervisor/bundle/get",
        supervisor::bundle::GetRequest {
            path: "assets/map.bin".to_owned(),
        },
        supervisor::bundle::GetResponse::Found {
            bytes: vec![1, 2, 3],
        },
    );
}

/// The identity endpoint answers with the bundle's own manifest document, so a
/// client decodes the same document every participant of this execution reads.
#[test]
fn the_info_reply_is_the_manifest_document_itself() {
    fn reply_is_the_manifest_document(
        reply: <supervisor::info::InfoRequest as QueryEndpoint>::Response,
    ) -> ManifestDocument {
        reply
    }

    let manifest = ManifestDocument::new(
        RobotBuilder::new("rover")
            .service("drive", None)
            .build()
            .expect("fixture robot"),
    );
    let encoded = MessagePack::encode(&manifest).expect("the manifest encodes");
    let decoded =
        MessagePack::decode::<<supervisor::info::InfoRequest as QueryEndpoint>::Response>(&encoded)
            .expect("the manifest decodes");
    let decoded = reply_is_the_manifest_document(decoded);
    assert_eq!(decoded.robot().id().as_str(), "rover");
    assert_eq!(
        MessagePack::encode(&decoded).expect("the decoded manifest encodes"),
        encoded,
        "the reply is the document, not a projection of it"
    );
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

/// A path can spell nothing but plain names and still leave the bundle, if
/// something under the root is a symlink out of it. What the endpoint serves is
/// decided by where the entry really is, not by how it was spelled.
#[cfg(unix)]
#[test]
fn bundle_entry_refuses_an_entry_that_resolves_outside_the_bundle() {
    let outside = tempfile::tempdir().expect("a directory outside the bundle");
    fs::write(outside.path().join("secret"), b"secret").expect("outside fixture");
    fs::create_dir(outside.path().join("elsewhere")).expect("outside directory");
    fs::write(outside.path().join("elsewhere/secret"), b"secret").expect("outside fixture");

    let root = tempfile::tempdir().expect("temporary bundle root");
    fs::write(root.path().join("manifest.json"), b"manifest").expect("manifest fixture");
    std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("escape"))
        .expect("a symlink out of the bundle");
    std::os::unix::fs::symlink(
        outside.path().join("elsewhere"),
        root.path().join("elsewhere"),
    )
    .expect("a symlinked directory out of the bundle");

    for refused in ["escape", "elsewhere/secret"] {
        assert_eq!(
            bundle_entry(root.path(), refused),
            supervisor::bundle::GetResponse::InvalidPath,
            "{refused:?}"
        );
    }
    // The bundle's own entries are unaffected by the check.
    assert_eq!(
        bundle_entry(root.path(), "manifest.json"),
        supervisor::bundle::GetResponse::Found {
            bytes: b"manifest".to_vec(),
        }
    );
}

/// Both host actions are accepted as asked, and the acceptance names the
/// revision the execution was at when the supervisor took the request.
#[test]
fn host_actions_are_accepted_at_the_current_revision() {
    let (state, _) = present_state();
    let revision = state.snapshot().revision;
    for (request, expected) in [
        (Command::Reboot, HostAction::Reboot),
        (Command::Poweroff, HostAction::Poweroff),
    ] {
        let (outcome, action) = command(&state, request);
        assert_eq!(
            outcome,
            CommandOutcome::Accepted {
                at_revision: revision
            }
        );
        assert_eq!(action, expected);
    }
}

/// One expected runtime, present under a known producer.
fn present_state() -> (ExecutionState, ParticipantId) {
    let robot = RobotBuilder::new("rover").build().expect("fixture robot");
    let state = ExecutionState::new(Presence::for_robot(&robot));
    let participant = ParticipantId::new("brain").expect("fixture participant");
    state.record_presence(
        &participant,
        ProducerId::try_from(1_u128 << 124).expect("fixture producer"),
        true,
    );
    (state, participant)
}

/// One query endpoint's rendered key and declared kind, read off the owner-side
/// topic the supervisor binds.
fn assert_query_endpoint<E: QueryEndpoint>(topic: &Topic<ServeQuery<E>>, key: &str) {
    assert_eq!(topic.key(), key);
    assert_eq!(
        <E::Semantics as EndpointSemantics>::KIND,
        EndpointKind::Query
    );
}

/// One stream endpoint's rendered key and declared kind.
fn assert_stream_endpoint<E: Endpoint>(topic: &Topic<Publish<E>>, key: &str) {
    assert_eq!(topic.key(), key);
    assert_eq!(
        <E::Semantics as EndpointSemantics>::KIND,
        EndpointKind::Stream
    );
}

fn assert_stream_round_trip<E>(topic: &Topic<Publish<E>>, key: &str, payload: E)
where
    E: Endpoint + Debug + PartialEq,
{
    assert_stream_endpoint(topic, key);
    let encoded = MessagePack::encode(&payload).expect("endpoint payload encodes");
    let decoded = MessagePack::decode::<E>(&encoded).expect("endpoint payload decodes");
    assert_eq!(decoded, payload);
}

fn assert_query_round_trip<E>(
    topic: &Topic<ServeQuery<E>>,
    key: &str,
    request: E,
    response: E::Response,
) where
    E: QueryEndpoint + Debug + PartialEq,
    E::Response: Debug + PartialEq,
{
    assert_query_endpoint(topic, key);
    let encoded = MessagePack::encode(&request).expect("endpoint request encodes");
    let decoded = MessagePack::decode::<E>(&encoded).expect("endpoint request decodes");
    assert_eq!(decoded, request);

    let encoded = MessagePack::encode(&response).expect("endpoint response encodes");
    let decoded = MessagePack::decode::<E::Response>(&encoded).expect("endpoint response decodes");
    assert_eq!(decoded, response);
}
