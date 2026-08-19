//! Shared test fixtures. No tests live here - every test lives with the module
//! whose behavior it exercises.
//!
//! What this module owns is a stand-in endpoint surface: plain payloads bound
//! to the crate-private [`TestFamily`] with the same sealed endpoint typing the
//! api tree emits. The bus is the ABI floor and must be testable without a
//! concrete api tree above it. Several modules (`abi`, `handle`, `session`,
//! `server`, `router`) need the same stand-in endpoints to exercise a real
//! end-to-end path, so they are declared once rather than five times.
//!
//! These hand-written endpoint bindings are the one exception to the rule that
//! only [`crate::endpoints!`] writes them, and they are why the workspace
//! policy gate exempts test sources from it.
//!
//! The golden tests that bind the bus to the real api tree are the crate's own
//! integration tests.

use crate::identity::{ExecutionId, ParticipantId, ProducerId, TimelineId};
use serde::{Deserialize, Serialize};
use zenoh::key_expr::KeyExpr;
use zenoh::sample::{Sample, SampleBuilder};

use crate::bus::abi::CodecId;
use crate::bus::contract::{Endpoint, TestFamily};
use crate::bus::handle::stamp::StepToken;
use crate::bus::metadata::{BusMetadata, ParticipantSourceIdentity, SourceAttribution};
use crate::bus::session::BusConfig;
use crate::bus::time::{RobotInstant, TimeWindow};
use crate::bus::tree::BoundEndpoint;

/// Bind a literal key to a stand-in endpoint, the way a path walk would.
///
/// The api tree is the only other caller of this constructor; a bus unit test
/// has no tree to walk, so it states the key it means.
pub(crate) fn bound<E: Endpoint>(key: &str) -> BoundEndpoint<E> {
    BoundEndpoint::new(key.to_owned())
}

/// The stand-in state key.
pub(crate) const TARGET_TOPIC: &str = "yTEST/drive/target";

/// The stand-in setpoint key.
pub(crate) const MANUAL_TOPIC: &str = "yTEST/motion/manual";

/// The stand-in query key.
pub(crate) const GET_TOPIC: &str = "yTEST/asset/get";

/// A state body: it is published at a logical step, so it carries robot time
/// and is subject to timeline barriers.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Target {
    pub(crate) linear_x_mps: f32,
    pub(crate) angular_z_radps: f32,
}

impl crate::bus::contract::sealed::Endpoint for Target {}

impl Endpoint for Target {
    type Family = TestFamily;
    type Semantics = crate::bus::State;
}

/// A command body, standing in for a leased control input: it expresses no
/// robot time, so it is never quarantined at a timeline barrier.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Manual {
    pub(crate) linear_x_mps: f32,
}

impl crate::bus::contract::sealed::Endpoint for Manual {}

impl Endpoint for Manual {
    type Family = TestFamily;
    type Semantics = crate::bus::Setpoint;
}

/// The request half of a stand-in query contract.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct GetRequest {
    pub(crate) path: String,
}

/// The response half of a stand-in query contract.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum GetResponse {
    Found { bytes: Vec<u8> },
    Missing,
}

impl crate::bus::contract::sealed::Endpoint for GetRequest {}

impl Endpoint for GetRequest {
    type Family = TestFamily;
    type Semantics = crate::bus::Query;
}

impl crate::bus::contract::QueryEndpoint for GetRequest {
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
            crate::identity::ParticipantId::new("tester").expect("test participant"),
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
