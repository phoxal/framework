//! Deterministic tests of the supervisor-owned endpoint decisions.
//!
//! A real client attachment crosses process, socket, and Zenoh boundaries and
//! is therefore host acceptance evidence, not a repository test. These tests
//! exercise the decisions immediately behind those endpoints without creating
//! another protocol model or opening a transport.

use std::fmt::Debug;
use std::fs;

use crate::bus::{
    Codec, Endpoint, EndpointKind, EndpointSemantics, MessagePack, Publish, QueryEndpoint,
    ServeQuery, Topic,
};
use crate::identity::{ParticipantId, ProducerId};
use crate::bundle::BundlePath;
use crate::model::builder::RobotBuilder;
use crate::model::manifest::ManifestDocument;
use crate::runtime::api as runtime;
use crate::supervisor::api as supervisor;
use crate::supervisor::api::command::{Command, CommandOutcome};
use crate::supervisor::api::connect::{ConnectReply, ConnectRequest};
use crate::version::FrameworkVersion;

use super::{
    MAX_BUNDLE_CHUNK_BYTES, HostAction, bundle_entry, classify_bundle_path_error, command,
    connect_reply,
};
use crate::supervisor::host::presence::Presence;
use crate::supervisor::host::state::ExecutionState;

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

    let domain = state.time_domain();
    assert_stream_round_trip(
        &api.time_domain().owner(),
        "supervisor/time_domain",
        supervisor::time_domain::TimeDomainStream { domain },
    );
    assert_query_round_trip(
        &api.time_domain().current().owner(),
        "supervisor/time_domain/current",
        supervisor::time_domain::CurrentRequest {},
        supervisor::time_domain::CurrentResponse { domain },
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
            path: bundle_path("assets/map.bin"),
            offset: 0,
        },
        supervisor::bundle::GetResponse::Chunk {
            bytes: vec![1, 2, 3],
            eof: true,
        },
    );
}

/// The identity endpoint wraps the immutable manifest and nothing dynamic, so
/// every participant decodes the same static document.
#[test]
fn the_info_reply_contains_the_manifest_document() {
    let manifest = ManifestDocument::new(
        RobotBuilder::new("rover")
            .service("drive", None)
            .build()
            .expect("fixture robot"),
    );
    let reply = supervisor::info::InfoResponse { manifest };
    let encoded = MessagePack::encode(&reply).expect("the execution info encodes");
    let decoded =
        MessagePack::decode::<<supervisor::info::InfoRequest as QueryEndpoint>::Response>(&encoded)
            .expect("the manifest decodes");
    assert_eq!(decoded.manifest.robot().id().as_str(), "rover");
    assert_eq!(
        MessagePack::encode(&decoded).expect("the decoded manifest encodes"),
        encoded,
        "the reply preserves the static execution document"
    );
}

#[test]
fn bundle_entry_serves_bounded_asset_ranges() {
    let root = tempfile::tempdir().expect("temporary bundle root");
    fs::write(root.path().join("manifest.json"), b"manifest").expect("manifest fixture");
    fs::create_dir(root.path().join("assets")).expect("asset directory");
    fs::write(root.path().join("assets/map.bin"), b"map").expect("asset fixture");

    assert_eq!(
        bundle_entry(root.path(), &request("manifest.json", 0)),
        supervisor::bundle::GetResponse::Refused
    );
    assert_eq!(
        bundle_entry(root.path(), &request("assets/map.bin", 0)),
        supervisor::bundle::GetResponse::Chunk {
            bytes: b"map".to_vec(),
            eof: true,
        }
    );
    assert_eq!(
        bundle_entry(root.path(), &request("assets/missing.bin", 0)),
        supervisor::bundle::GetResponse::Missing
    );
    assert_eq!(
        bundle_entry(root.path(), &request("assets/map.bin", 3)),
        supervisor::bundle::GetResponse::Chunk {
            bytes: Vec::new(),
            eof: true,
        }
    );
}

/// The supervisor, rather than each caller, sets the largest reply. A caller
/// advances its requested offset by the bytes it received and therefore never
/// needs the size as a protocol field.
#[test]
fn bundle_entry_splits_a_large_asset_at_the_fixed_supervisor_bound() {
    let root = tempfile::tempdir().expect("temporary bundle root");
    fs::create_dir(root.path().join("assets")).expect("asset directory");
    let bytes = vec![9_u8; MAX_BUNDLE_CHUNK_BYTES + 1];
    fs::write(root.path().join("assets/large.bin"), bytes).expect("large asset fixture");

    let first = bundle_entry(root.path(), &request("assets/large.bin", 0));
    assert!(matches!(
        first,
        supervisor::bundle::GetResponse::Chunk {
            ref bytes,
            eof: false,
        } if bytes.len() == MAX_BUNDLE_CHUNK_BYTES
    ));
    assert_eq!(
        bundle_entry(
            root.path(),
            &request("assets/large.bin", MAX_BUNDLE_CHUNK_BYTES as u64)
        ),
        supervisor::bundle::GetResponse::Chunk {
            bytes: vec![9],
            eof: true,
        }
    );
}

/// A decoded request can name an existing directory or fail to resolve through
/// an existing non-directory. Neither is a missing asset, and an unreadable
/// entry must not be reported as absent.
#[test]
fn bundle_entry_classifies_invalid_and_unservable_paths_without_hiding_them_as_missing() {
    let root = tempfile::tempdir().expect("temporary bundle root");
    fs::create_dir(root.path().join("assets")).expect("asset directory");
    fs::create_dir(root.path().join("assets/maps")).expect("nested asset directory");
    fs::write(root.path().join("assets/map.bin"), b"map").expect("asset fixture");

    assert_eq!(
        bundle_entry(root.path(), &request("assets/maps", 0)),
        supervisor::bundle::GetResponse::InvalidPath
    );
    assert_eq!(
        bundle_entry(root.path(), &request("assets/map.bin/child", 0)),
        supervisor::bundle::GetResponse::InvalidPath
    );
    assert_eq!(
        classify_bundle_path_error(&std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        supervisor::bundle::GetResponse::Refused
    );
    assert_eq!(
        classify_bundle_path_error(&std::io::Error::from(std::io::ErrorKind::NotFound)),
        supervisor::bundle::GetResponse::Missing
    );

    let unavailable_root = root.path().join("unavailable");
    assert_eq!(
        bundle_entry(&unavailable_root, &request("assets/map.bin", 0)),
        supervisor::bundle::GetResponse::Refused
    );
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
    fs::create_dir(root.path().join("assets")).expect("asset directory");
    std::os::unix::fs::symlink(
        root.path().join("assets/missing.bin"),
        root.path().join("assets/dangling.bin"),
    )
    .expect("a dangling asset symlink");
    std::os::unix::fs::symlink(
        root.path().join("assets/missing-directory"),
        root.path().join("assets/dangling-directory"),
    )
    .expect("a dangling asset directory symlink");
    std::os::unix::fs::symlink(
        outside.path().join("elsewhere"),
        root.path().join("assets/outside-directory"),
    )
    .expect("an outside asset directory symlink");
    std::os::unix::fs::symlink(
        root.path().join("assets/loop-b"),
        root.path().join("assets/loop-a"),
    )
    .expect("the first symlink loop entry");
    std::os::unix::fs::symlink(
        root.path().join("assets/loop-a"),
        root.path().join("assets/loop-b"),
    )
    .expect("the second symlink loop entry");

    for refused in ["escape", "elsewhere/secret"] {
        assert_eq!(
            bundle_entry(root.path(), &request(refused, 0)),
            supervisor::bundle::GetResponse::InvalidPath,
            "{refused:?}"
        );
    }
    // The bundle's own entries are unaffected by the check.
    assert_eq!(
        bundle_entry(root.path(), &request("manifest.json", 0)),
        supervisor::bundle::GetResponse::Refused
    );
    assert_eq!(
        bundle_entry(root.path(), &request("assets/dangling.bin", 0)),
        supervisor::bundle::GetResponse::InvalidPath
    );
    for invalid in [
        "assets/dangling-directory/child.bin",
        "assets/outside-directory/missing.bin",
        "assets/loop-a",
    ] {
        assert_eq!(
            bundle_entry(root.path(), &request(invalid, 0)),
            supervisor::bundle::GetResponse::InvalidPath,
            "{invalid:?}"
        );
    }
}

fn bundle_path(path: &str) -> BundlePath {
    BundlePath::new(path).expect("a canonical test bundle path")
}

fn request(path: &str, offset: u64) -> supervisor::bundle::GetRequest {
    supervisor::bundle::GetRequest {
        path: bundle_path(path),
        offset,
    }
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
    let state = ExecutionState::new(Presence::for_robot(&robot))
        .expect("a fresh execution state accepts its initial time domain");
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
