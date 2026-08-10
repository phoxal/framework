//! Shared test fixtures. No tests live here - every test lives with the module
//! whose behavior it exercises.
//!
//! What this module owns is a stand-in endpoint surface: a hand-written
//! [`ApiFamily`] plus plain payloads and endpoint descriptors. `phoxal-bus`
//! is the ABI floor and must be testable without a concrete generated API.
//! Several modules (`abi`, `handle`, `session`, `server`, `router`) need the
//! same stand-in endpoints to exercise a real end-to-end path, so they are
//! declared once rather than five times.
//!
//! The golden tests that bind the bus to the real generated tree live in the
//! `phoxal` crate.

use phoxal_runtime_contract::identity::{ExecutionId, ParticipantId, ProducerId, TimelineId};
use serde::{Deserialize, Serialize};
use zenoh::key_expr::KeyExpr;
use zenoh::sample::{Sample, SampleBuilder};

use crate::abi::CodecId;
use crate::contract::{
    ApiFamily, EndpointDescriptor, EndpointKind, QueryEndpointDescriptor, SetpointContract,
    StateContract,
};
use crate::handle::stamp::StepToken;
use crate::metadata::{BusMetadata, ParticipantSourceIdentity, SourceAttribution};
use crate::session::BusConfig;
use crate::time::{RobotInstant, TimeWindow};

/// The stand-in API identity used by test endpoint descriptors.
pub(crate) enum TestApi {}

impl ApiFamily for TestApi {
    const ID: &'static str = "yTEST";
}

/// A state body: it is published at a logical step, so it carries robot time
/// and is subject to timeline barriers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Target {
    pub(crate) linear_x_mps: f32,
    pub(crate) angular_z_radps: f32,
}

pub(crate) struct TargetEndpoint;

impl EndpointDescriptor for TargetEndpoint {
    type Api = TestApi;
    type Payload = Target;
    const NAME: &'static str = "yTEST::drive::Target";
    const FAMILY: &'static str = "yTEST";
    const CONTRACT: &'static str = "drive::Target";
    const TOPIC: &'static str = "yTEST/drive/target";
    const KIND: EndpointKind = EndpointKind::State;
}

impl StateContract for TargetEndpoint {}

/// A command body, standing in for a leased control input: it expresses no
/// robot time, so it is never quarantined at a timeline barrier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Manual {
    pub(crate) linear_x_mps: f32,
}

pub(crate) struct ManualEndpoint;

impl EndpointDescriptor for ManualEndpoint {
    type Api = TestApi;
    type Payload = Manual;
    const NAME: &'static str = "yTEST::motion::Manual";
    const FAMILY: &'static str = "yTEST";
    const CONTRACT: &'static str = "motion::Manual";
    const TOPIC: &'static str = "yTEST/motion/manual";
    const KIND: EndpointKind = EndpointKind::Setpoint;
}

impl SetpointContract for ManualEndpoint {}

/// The request half of a stand-in query contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct GetRequest {
    pub(crate) path: String,
}

pub(crate) struct GetEndpoint;

impl EndpointDescriptor for GetEndpoint {
    type Api = TestApi;
    type Payload = GetRequest;
    const NAME: &'static str = "yTEST::asset::GetRequest";
    const FAMILY: &'static str = "yTEST";
    const CONTRACT: &'static str = "asset::GetRequest";
    const TOPIC: &'static str = "yTEST/asset/get";
    const KIND: EndpointKind = EndpointKind::Query;
}

/// The response half of a stand-in query contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum GetResponse {
    Found { bytes: Vec<u8> },
    Missing,
}

impl QueryEndpointDescriptor for GetEndpoint {
    type Request = GetRequest;
    type Response = GetResponse;
}

/// A named test timeline. Production timelines are minted opaquely, so tests
/// that need to talk about a *specific* world history name theirs.
pub(crate) fn timeline(value: u64) -> TimelineId {
    TimelineId::from_raw(value).expect("a test timeline id is nonzero")
}

/// A distinct deterministic test producer. Production sessions mint their
/// producer through the bus owner, while tests name theirs explicitly.
pub(crate) fn producer(value: u128) -> ProducerId {
    ProducerId::try_from((1_u128 << 124) | value).expect("a test producer is canonical")
}

/// A participant config for an in-process unit-test fabric.
pub(crate) fn participant_config(participant: impl Into<String>) -> BusConfig {
    BusConfig::for_participant(
        ExecutionId::mint(),
        ParticipantId::new(participant).expect("valid test participant"),
        Vec::new(),
    )
}

/// A step token on a named timeline.
pub(crate) fn step(line: u64, ticks: u64) -> StepToken {
    StepToken::mint(RobotInstant::new(timeline(line), ticks))
}

/// Provenance for a hand-built sample.
pub(crate) fn metadata() -> BusMetadata {
    BusMetadata {
        codec: CodecId::MessagePack.as_u8(),
        sequence: 7,
        stream_position: None,
        produced_at: Some(TimeWindow::exact(RobotInstant::new(timeline(1), 42))),
        source: SourceAttribution::Participant(ParticipantSourceIdentity::new(
            phoxal_runtime_contract::identity::ParticipantId::new("tester")
                .expect("test participant"),
            producer(1),
        )),
    }
}

/// A sample carrying `payload`, whose encoding string and attachment both name
/// `codec`.
pub(crate) fn sample_with(codec: u8, payload: Vec<u8>) -> Sample {
    let encoding = if codec == CodecId::MessagePack.as_u8() {
        CodecId::MessagePack.encoding_string()
    } else {
        format!("phoxal/v0;codec={codec}")
    };
    sample_with_encoding(codec, encoding, payload)
}

/// A sample whose encoding string and attachment codec are set independently,
/// so a test can make the two disagree.
pub(crate) fn sample_with_encoding(codec: u8, encoding: String, payload: Vec<u8>) -> Sample {
    let mut meta = metadata();
    meta.codec = codec;
    let key: KeyExpr<'static> =
        KeyExpr::try_from("phoxal/dead/yTEST/drive/target").expect("a legal test key");
    SampleBuilder::put(key, payload)
        .encoding(encoding)
        .attachment(meta.encode().expect("test metadata encodes"))
        .into()
}

/// A well-formed [`Target`] sample under `codec`.
pub(crate) fn sample(codec: u8) -> Sample {
    let payload = rmp_serde::to_vec_named(&Target {
        linear_x_mps: 1.0,
        angular_z_radps: 0.5,
    })
    .expect("a test body encodes");
    sample_with(codec, payload)
}

/// A short unix-socket endpoint plus the directory that owns it.
///
/// Unix-socket addresses are length-limited and the macOS default temp dir is
/// long enough to overflow the limit, so these bind under `/tmp` directly.
#[cfg(feature = "router")]
pub(crate) fn socket_endpoint(prefix: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .expect("short-path temp dir for the unix socket");
    let endpoint = format!("unixsock-stream/{}", dir.path().join("r.sock").display());
    (dir, endpoint)
}
