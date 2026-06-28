//! Bus golden + live tests: the `bus_abi` slots (encoding string, metadata,
//! codec id), the `<namespace>/robots/<robot-id>/<typed-key>` root + namespace
//! validation (D38/D43b), the `api_version` mismatch health event, and a live
//! in-process Publisher → Latest round-trip.

use std::time::Duration;

use serial_test::serial;
use zenoh::key_expr::KeyExpr;
use zenoh::sample::SampleBuilder;

use crate::api::ContractBody;
use crate::api::y2026_1 as api;
use crate::bus::abi::{CodecId, encoding_string};
use crate::bus::handle::decode_sample;
use crate::bus::metadata::{BusMetadata, Source};
use crate::bus::{
    BUS_ABI, Bus, BusConfig, BusError, LogicalTime, Querier, QueryCode, QueryError, QueryFailure,
};

fn metadata(api_version: &str) -> BusMetadata {
    BusMetadata {
        api_version: api_version.to_string(),
        family: <api::drive::Target as crate::api::ContractBody>::FAMILY.to_string(),
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

fn sample(api_version: &str, codec: u8) -> zenoh::sample::Sample {
    let body = api::drive::Target {
        linear_x_mps: 1.0,
        angular_z_radps: 0.5,
        curvature_limit_radpm: None,
    };
    let mut meta = metadata(api_version);
    meta.codec = codec;
    let payload = rmp_serde::to_vec_named(&body).unwrap();
    let key: KeyExpr<'static> = KeyExpr::try_from("dev/robots/r1/drive/target").unwrap();
    SampleBuilder::put(key, payload)
        .encoding(encoding_string(
            meta.family.as_str(),
            api_version,
            CodecId::MessagePack,
        ))
        .attachment(meta.encode())
        .into()
}

#[test]
fn bus_abi_id_is_frozen() {
    assert_eq!(BUS_ABI.id(), "phoxal-bus/v0");
}

#[test]
fn encoding_string_mirrors_metadata_slots() {
    let enc = encoding_string("drive::State", "y2026_1", CodecId::MessagePack);
    assert_eq!(enc, "phoxal/v0;family=drive::State;api=y2026_1;codec=1");
}

#[test]
fn metadata_round_trips() {
    let meta = metadata("y2026_1");
    let bytes = meta.encode();
    assert_eq!(BusMetadata::decode(&bytes).unwrap(), meta);
}

#[test]
fn decode_accepts_matching_api_version() {
    let s = sample("y2026_1", CodecId::MessagePack.as_u8());
    let (body, meta) = decode_sample::<api::drive::Target>(&s, "drive/target", "y2026_1").unwrap();
    assert_eq!(body.linear_x_mps, 1.0);
    assert_eq!(meta.api_version, "y2026_1");
}

#[test]
fn decode_rejects_api_version_mismatch() {
    let s = sample("not-y2026_1", CodecId::MessagePack.as_u8());
    let err = decode_sample::<api::drive::Target>(&s, "drive/target", "y2026_1").unwrap_err();
    assert!(matches!(err, BusError::ApiVersionMismatch { .. }));
}

#[test]
fn decode_rejects_unsupported_codec() {
    let s = sample("y2026_1", 99);
    let err = decode_sample::<api::drive::Target>(&s, "drive/target", "y2026_1").unwrap_err();
    assert!(matches!(err, BusError::UnsupportedCodec(99, _)));
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
    let topic = api::topic::new().drive().target();

    let publisher = crate::bus::Publisher::<api::drive::Target>::new(bus.clone(), &topic).unwrap();
    let latest = crate::bus::Latest::<api::drive::Target>::new(&bus, &topic)
        .await
        .unwrap();

    publisher
        .publish_at(
            LogicalTime::new(0, 100),
            api::drive::Target {
                linear_x_mps: 0.9,
                angular_z_radps: -0.1,
                curvature_limit_radpm: None,
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
            let response = api::asset::GetResponse::Found {
                bytes: vec![9, 9, 9],
            };
            let payload = rmp_serde::to_vec_named(&response).unwrap();
            incoming
                .reply(
                    &server_bus,
                    payload,
                    <api::asset::GetResponse as ContractBody>::FAMILY,
                    "y2026_1",
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

    let querier = Querier::<api::asset::GetRequest, api::asset::GetResponse>::new(
        bus.clone(),
        &api::topic::new().asset().get(),
        Duration::from_secs(5),
    )
    .unwrap();

    let ok = querier
        .query(api::asset::GetRequest {
            path: "a".to_string(),
        })
        .await
        .expect("first query should succeed");
    assert!(matches!(ok, api::asset::GetResponse::Found { .. }));

    let err = querier
        .query(api::asset::GetRequest {
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
