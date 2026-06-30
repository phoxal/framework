//! Pure-bus-mechanic tests: the `bus_abi` slots (encoding string, metadata, codec
//! id), the `<namespace>/robots/<robot-id>/<typed-key>` root + namespace
//! validation (D38/D43b), the `api_version`/family/codec fast-reject in
//! `decode_sample`, and a live in-process Publisher → Latest round-trip.
//!
//! These exercise the bus client against a hand-written [`ContractBody`] (no
//! `phoxal_api_tree!`, which lives in the `phoxal-api` crate): phoxal-bus is
//! the ABI floor and must be testable without the concrete dated API versions.
//! The golden tests that bind the bus to the real `y2026_1` tree live in the
//! `phoxal` crate (`phoxal/tests/bus_api.rs`).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serial_test::serial;
use zenoh::bytes::Encoding;
use zenoh::key_expr::KeyExpr;
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::sample::SampleBuilder;

use crate::abi::{CodecId, encoding_string, parse_encoding_string};
use crate::codec::CodecError;
use crate::contract::{ApiVersion, ContractBody};
use crate::handle::decode_sample;
use crate::metadata::{BusMetadata, Source};
use crate::topic::Topic;
use crate::{
    AskQuery, BUS_ABI, Bus, BusConfig, BusError, Latest, LogicalTime, Publish, Publisher, Querier,
    QueryCode, QueryError, QueryFailure, Subscribe,
};

// A hand-written API version + contract body, standing in for the macro-generated
// `y2026_1` tree (which lives in `phoxal`). The bus client is generic over these
// traits; that is all it needs to be exercised end to end.
enum TestApi {}
impl ApiVersion for TestApi {
    const ID: &'static str = "yTEST";
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Target {
    linear_x_mps: f32,
    angular_z_radps: f32,
}
impl ContractBody for Target {
    type Api = TestApi;
    const FAMILY: &'static str = "drive::Target";
    const TOPIC: &'static str = "drive/target";
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct OtherBody {
    flag: bool,
}
impl ContractBody for OtherBody {
    type Api = TestApi;
    const FAMILY: &'static str = "safety::Status";
    const TOPIC: &'static str = "safety/state";
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GetRequest {
    path: String,
}
impl ContractBody for GetRequest {
    type Api = TestApi;
    const FAMILY: &'static str = "asset::GetRequest";
    const TOPIC: &'static str = "asset/get";
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum GetResponse {
    Found { bytes: Vec<u8> },
    Missing,
}
impl ContractBody for GetResponse {
    type Api = TestApi;
    const FAMILY: &'static str = "asset::GetResponse";
    const TOPIC: &'static str = "asset/get";
}

fn metadata(api_version: &str) -> BusMetadata {
    BusMetadata {
        api_version: api_version.to_string(),
        family: <Target as ContractBody>::FAMILY.to_string(),
        codec: CodecId::MessagePack.as_u8(),
        produced_at_ns: 42,
        epoch: 0,
        source: Source {
            participant: "tester".to_string(),
            incarnation: 1,
            sequence: 7,
        },
    }
}

fn sample_with(
    api_version: &str,
    codec: u8,
    family: &str,
    payload: Vec<u8>,
) -> zenoh::sample::Sample {
    let encoding = if codec == CodecId::MessagePack.as_u8() {
        encoding_string(family, api_version, CodecId::MessagePack)
    } else {
        format!("phoxal/v0;family={family};api={api_version};codec={codec}")
    };
    sample_with_encoding(api_version, codec, family, encoding, payload)
}

fn sample_with_encoding(
    api_version: &str,
    codec: u8,
    family: &str,
    encoding: String,
    payload: Vec<u8>,
) -> zenoh::sample::Sample {
    let mut meta = metadata(api_version);
    meta.codec = codec;
    meta.family = family.to_string();
    let key: KeyExpr<'static> = KeyExpr::try_from("dev/robots/r1/drive/target").unwrap();
    SampleBuilder::put(key, payload)
        .encoding(encoding)
        .attachment(meta.encode())
        .into()
}

fn sample(api_version: &str, codec: u8) -> zenoh::sample::Sample {
    let body = Target {
        linear_x_mps: 1.0,
        angular_z_radps: 0.5,
    };
    let payload = rmp_serde::to_vec_named(&body).unwrap();
    sample_with(
        api_version,
        codec,
        <Target as ContractBody>::FAMILY,
        payload,
    )
}

#[test]
fn bus_abi_id_is_frozen() {
    assert_eq!(BUS_ABI.id(), "phoxal-bus/v0");
}

#[test]
fn encoding_string_mirrors_metadata_slots() {
    let enc = encoding_string("drive::State", "y2026_1", CodecId::MessagePack);
    assert_eq!(enc, "phoxal/v0;family=drive::State;api=y2026_1;codec=1");
    let parsed = parse_encoding_string(&enc).unwrap();
    assert_eq!(parsed.family, "drive::State");
    assert_eq!(parsed.api_version, "y2026_1");
    assert_eq!(parsed.codec_id(), Some(CodecId::MessagePack));
}

#[test]
fn encoding_string_includes_body_family_id() {
    let enc = encoding_string(
        <Target as ContractBody>::FAMILY,
        <<Target as ContractBody>::Api as ApiVersion>::ID,
        CodecId::MessagePack,
    );
    assert!(enc.contains("family=drive::Target"));
    assert_eq!(enc, "phoxal/v0;family=drive::Target;api=yTEST;codec=1");
}

#[test]
fn metadata_round_trips() {
    let meta = metadata("y2026_1");
    let bytes = meta.encode();
    assert_eq!(BusMetadata::decode(&bytes).unwrap(), meta);
}

#[test]
fn decode_accepts_matching_api_version() {
    let s = sample("yTEST", CodecId::MessagePack.as_u8());
    let (body, meta) = decode_sample::<Target>(&s, "drive/target", "yTEST").unwrap();
    assert_eq!(body.linear_x_mps, 1.0);
    assert_eq!(meta.api_version, "yTEST");
}

#[test]
fn decode_rejects_api_version_mismatch() {
    let s = sample("not-yTEST", CodecId::MessagePack.as_u8());
    let err = decode_sample::<Target>(&s, "drive/target", "yTEST").unwrap_err();
    assert!(matches!(err, BusError::ApiVersionMismatch { .. }));
}

#[test]
fn decode_rejects_family_mismatch() {
    let s = sample("yTEST", CodecId::MessagePack.as_u8());
    let err = decode_sample::<OtherBody>(&s, "safety/state", "yTEST").unwrap_err();
    match err {
        BusError::Metadata { detail, .. } => {
            assert!(detail.contains("encoding family mismatch"));
        }
        other => panic!("expected family mismatch metadata error, got {other:?}"),
    }
}

#[test]
fn decode_rejects_encoding_api_mismatch_before_body_decode() {
    let body = Target {
        linear_x_mps: 1.0,
        angular_z_radps: 0.5,
    };
    let payload = rmp_serde::to_vec_named(&body).unwrap();
    let s = sample_with_encoding(
        "yTEST",
        CodecId::MessagePack.as_u8(),
        <Target as ContractBody>::FAMILY,
        encoding_string(
            <Target as ContractBody>::FAMILY,
            "not-yTEST",
            CodecId::MessagePack,
        ),
        payload,
    );

    let err = decode_sample::<Target>(&s, "drive/target", "yTEST").unwrap_err();
    assert!(matches!(err, BusError::ApiVersionMismatch { .. }));
}

#[test]
fn decode_rejects_encoding_attachment_mismatch_before_body_decode() {
    let body = Target {
        linear_x_mps: 1.0,
        angular_z_radps: 0.5,
    };
    let payload = rmp_serde::to_vec_named(&body).unwrap();
    let s = sample_with_encoding(
        "yTEST",
        CodecId::MessagePack.as_u8(),
        "safety::Status",
        encoding_string(
            <Target as ContractBody>::FAMILY,
            "yTEST",
            CodecId::MessagePack,
        ),
        payload,
    );

    let err = decode_sample::<Target>(&s, "drive/target", "yTEST").unwrap_err();
    match err {
        BusError::Metadata { detail, .. } => {
            assert!(detail.contains("encoding/BusMetadata mismatch"));
        }
        other => panic!("expected encoding/metadata mismatch, got {other:?}"),
    }
}

#[test]
fn decode_rejects_unsupported_codec() {
    let s = sample("yTEST", 99);
    let err = decode_sample::<Target>(&s, "drive/target", "yTEST").unwrap_err();
    assert!(matches!(err, BusError::UnsupportedCodec(99, _)));
}

#[test]
fn decode_rejects_corrupt_payload() {
    let s = sample_with(
        "yTEST",
        CodecId::MessagePack.as_u8(),
        <Target as ContractBody>::FAMILY,
        vec![0xc1, 0xc1, 0xc1],
    );
    let err = decode_sample::<Target>(&s, "drive/target", "yTEST").unwrap_err();
    assert!(matches!(err, BusError::Codec(CodecError::Decode(_))));
}

#[test]
fn every_query_code_round_trips() {
    let codes = [
        QueryCode::NotFound,
        QueryCode::InvalidArgument,
        QueryCode::Internal,
        QueryCode::Unavailable,
        QueryCode::Unimplemented,
        QueryCode::DeadlineExceeded,
    ];
    for code in codes {
        let failure = QueryFailure::new(code, format!("{code:?}"));
        assert_eq!(QueryFailure::decode(&failure.encode()).unwrap(), failure);
    }
}

#[test]
fn query_failure_details_round_trip() {
    let mut failure = QueryFailure::internal("extra detail");
    failure.details = Some(vec![1, 2, 3, 4]);
    failure.details_encoding = Some("application/phoxal-test".to_string());

    let decoded = QueryFailure::decode(&failure.encode()).unwrap();
    assert_eq!(decoded, failure);
}

#[tokio::test]
async fn namespace_must_be_concrete_non_wildcard() {
    let err = Bus::open(BusConfig::in_process("dev/*", "r1"))
        .await
        .unwrap_err();
    assert!(matches!(err, BusError::Namespace(_)));

    let err = Bus::open(BusConfig::in_process("", "r1"))
        .await
        .unwrap_err();
    assert!(matches!(err, BusError::Namespace(_)));
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn key_root_is_namespace_robots_robot_id() {
    let bus = Bus::open(BusConfig::in_process("dev", "r1")).await.unwrap();
    assert_eq!(bus.root(), "dev/robots/r1");
    assert_eq!(bus.full_key("drive/state"), "dev/robots/r1/drive/state");
    bus.close().await.unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_publisher_to_latest_round_trip() {
    let bus = Bus::open(BusConfig::in_process("dev", "rt")).await.unwrap();
    let pub_topic = Topic::<Publish<Target>>::new_static(<Target as ContractBody>::TOPIC);
    let sub_topic = Topic::<Subscribe<Target>>::new_static(<Target as ContractBody>::TOPIC);

    let publisher = Publisher::<Target>::new(bus.clone(), &pub_topic).unwrap();
    let latest = Latest::<Target>::new(&bus, &sub_topic).await.unwrap();

    publisher
        .publish_at(
            LogicalTime::new(0, 100),
            Target {
                linear_x_mps: 0.9,
                angular_z_radps: -0.1,
            },
        )
        .await
        .unwrap();

    let mut received = None;
    for _ in 0..50 {
        if let Some(body) = latest.latest() {
            received = Some(body);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let body = received.expect("Latest should observe the published body in-process");
    assert_eq!(body.linear_x_mps, 0.9);

    bus.close().await.unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn publish_at_reports_closed_bus() {
    let bus = Bus::open(BusConfig::in_process("dev", "closed"))
        .await
        .unwrap();
    let topic = Topic::<Publish<Target>>::new_static(<Target as ContractBody>::TOPIC);
    let publisher = Publisher::<Target>::new(bus.clone(), &topic).unwrap();
    bus.close().await.unwrap();

    let err = publisher
        .publish_at(
            LogicalTime::new(0, 100),
            Target {
                linear_x_mps: 0.9,
                angular_z_radps: -0.1,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, BusError::Closed));
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_query_timeout_maps_to_deadline_exceeded() {
    let bus = Bus::open(BusConfig::in_process("dev", "timeout"))
        .await
        .unwrap();
    let server = bus.declare_server("asset/get").await.unwrap();

    let server_task = tokio::spawn(async move {
        let _incoming = server.recv().await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let topic =
        Topic::<AskQuery<GetRequest, GetResponse>>::new_static(<GetRequest as ContractBody>::TOPIC);
    let querier =
        Querier::<GetRequest, GetResponse>::new(bus.clone(), &topic, Duration::from_millis(20))
            .unwrap();

    let err = querier
        .query(GetRequest {
            path: "slow".to_string(),
        })
        .await
        .expect_err("query should time out");
    match err {
        QueryError::Timeout(failure) => assert_eq!(failure.code, QueryCode::DeadlineExceeded),
        other => panic!("expected QueryError::Timeout, got {other:?}"),
    }

    server_task.await.unwrap();
    bus.close().await.unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incoming_query_rejects_encoding_attachment_mismatch() {
    let bus = Bus::open(BusConfig::in_process("dev", "q-mismatch"))
        .await
        .unwrap();
    let server = bus.declare_server("asset/get").await.unwrap();

    let request = GetRequest {
        path: "asset.bin".to_string(),
    };
    let payload = rmp_serde::to_vec_named(&request).unwrap();
    let meta = BusMetadata {
        api_version: "yTEST".to_string(),
        family: <GetRequest as ContractBody>::FAMILY.to_string(),
        codec: CodecId::MessagePack.as_u8(),
        produced_at_ns: 0,
        epoch: 0,
        source: Source {
            participant: "tester".to_string(),
            incarnation: 1,
            sequence: 1,
        },
    };

    let key = OwnedKeyExpr::new(bus.full_key("asset/get")).unwrap();
    let _replies = bus
        .session()
        .get(key)
        .payload(payload)
        .encoding(Encoding::from(encoding_string(
            "drive::Target",
            "yTEST",
            CodecId::MessagePack,
        )))
        .attachment(meta.encode())
        .target(zenoh::query::QueryTarget::All)
        .consolidation(zenoh::query::ConsolidationMode::None)
        .await
        .unwrap();

    let incoming = server.recv().await.unwrap();
    let err = incoming.request_metadata().unwrap_err();
    match err {
        BusError::Metadata { detail, .. } => {
            assert!(detail.contains("request encoding/BusMetadata mismatch"));
        }
        other => panic!("expected request encoding/metadata mismatch, got {other:?}"),
    }

    bus.close().await.unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_query_round_trip_ok_then_error() {
    let bus = Bus::open(BusConfig::in_process("dev", "q")).await.unwrap();
    let server = bus.declare_server("asset/get").await.unwrap();
    let server_bus = bus.clone();

    let server_task = tokio::spawn(async move {
        // First query → a Found response. Scoped so the query is dropped right
        // after replying, letting the complete queryable's reply stream close.
        {
            let incoming = server.recv().await.unwrap();
            let response = GetResponse::Found {
                bytes: vec![9, 9, 9],
            };
            let payload = rmp_serde::to_vec_named(&response).unwrap();
            incoming
                .reply(
                    &server_bus,
                    payload,
                    <GetResponse as ContractBody>::FAMILY,
                    "yTEST",
                )
                .await
                .unwrap();
        }

        // Second query → a structured error on the native error leg.
        {
            let incoming = server.recv().await.unwrap();
            incoming
                .reply_err(&QueryFailure::not_found("no such asset"))
                .await
                .unwrap();
        }
    });

    let topic =
        Topic::<AskQuery<GetRequest, GetResponse>>::new_static(<GetRequest as ContractBody>::TOPIC);
    let querier =
        Querier::<GetRequest, GetResponse>::new(bus.clone(), &topic, Duration::from_secs(5))
            .unwrap();

    let ok = querier
        .query(GetRequest {
            path: "a".to_string(),
        })
        .await
        .expect("first query should succeed");
    assert!(matches!(ok, GetResponse::Found { .. }));

    let err = querier
        .query(GetRequest {
            path: "b".to_string(),
        })
        .await
        .expect_err("second query should be a server error");
    match err {
        QueryError::Server(failure) => assert_eq!(failure.code, QueryCode::NotFound),
        other => panic!("expected QueryError::Server, got {other:?}"),
    }

    server_task.await.unwrap();
    bus.close().await.unwrap();
}
