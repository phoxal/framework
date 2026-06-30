//! Macro server-dispatch tests: the generated `__serve_exclusive` /
//! `__serve_snapshot` decode the request, call the handler, and encode the reply
//! (or a `QueryFailure`). These exercise the macro output directly, without a bus.
#![allow(clippy::assertions_on_constants)]

use std::sync::Arc;

use crate::bus::{ApiVersion, Codec, ContractBody, MessagePack, QueryCode};
use crate::prelude::*;
use crate::runtime::RuntimeBehavior;
use phoxal_api::y2026_1 as api;

#[derive(phoxal::Runtime)]
#[phoxal(id = "asset-test", api = y2026_1)]
struct AssetTest {
    present: bool,
}

#[phoxal::runtime]
impl AssetTest {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self { present: true })
    }

    #[server(topic = api::topic::new().asset().get())]
    async fn get(
        &mut self,
        request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        if self.present && request.path == "ok" {
            Ok(api::asset::GetResponse::Found {
                bytes: vec![1, 2, 3],
            })
        } else {
            Err(phoxal::bus::QueryFailure::not_found("no such asset"))
        }
    }
}

#[tokio::test]
async fn exclusive_server_dispatch_ok_error_and_unknown() {
    let mut rt = AssetTest { present: true };

    // The declared topic shows up in the metadata.
    assert_eq!(AssetTest::__exclusive_server_topics(), &["asset/get"]);
    assert!(AssetTest::__snapshot_server_topics().is_empty());
    AssetTest::__validate_server_topics().unwrap();
    assert!(!AssetTest::HAS_SNAPSHOT);
    assert!(
        AssetTest::SERVER_CONTRACTS
            .iter()
            .any(|c| c.family == <api::asset::GetRequest as ContractBody>::FAMILY)
    );

    let api = <<api::asset::GetRequest as ContractBody>::Api as ApiVersion>::ID;
    let family = <api::asset::GetRequest as ContractBody>::FAMILY;

    let request = MessagePack::encode(&api::asset::GetRequest {
        path: "ok".to_string(),
    })
    .unwrap();
    let reply = rt
        .__serve_exclusive("asset/get", api, family, &request)
        .await
        .unwrap();
    let response: api::asset::GetResponse = MessagePack::decode(&reply.payload).unwrap();
    assert!(matches!(response, api::asset::GetResponse::Found { .. }));
    assert_eq!(
        reply.family,
        <api::asset::GetResponse as ContractBody>::FAMILY
    );

    let request = MessagePack::encode(&api::asset::GetRequest {
        path: "missing".to_string(),
    })
    .unwrap();
    let failure = rt
        .__serve_exclusive("asset/get", api, family, &request)
        .await
        .unwrap_err();
    assert_eq!(failure.code, QueryCode::NotFound);

    // Wrong request api_version is rejected before the handler runs.
    let failure = rt
        .__serve_exclusive("asset/get", "not-y2026_1", family, &request)
        .await
        .unwrap_err();
    assert_eq!(failure.code, QueryCode::InvalidArgument);

    let failure = rt
        .__serve_exclusive("other/topic", api, family, &request)
        .await
        .unwrap_err();
    assert_eq!(failure.code, QueryCode::Unimplemented);
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "bad-topic-test", api = y2026_1)]
struct BadTopicTest {}

#[phoxal::runtime]
impl BadTopicTest {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server(topic = phoxal::bus::Topic::<phoxal::bus::AskQuery<api::asset::GetRequest, api::asset::GetResponse>>::new_static("asset/other"))]
    async fn get(
        &mut self,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

#[test]
fn explicit_server_topic_key_must_match_request_body_topic() {
    let err = BadTopicTest::__validate_server_topics().unwrap_err();
    assert!(err.contains("does not match request body topic"));
    assert!(err.contains("asset/other"));
    assert!(err.contains("asset/get"));
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "duplicate-server-topic-test", api = y2026_1)]
struct DuplicateServerTopicTest {}

#[phoxal::runtime]
impl DuplicateServerTopicTest {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server(topic = api::topic::new().asset().get())]
    async fn get_one(
        &mut self,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }

    #[server(topic = api::topic::new().asset().get())]
    async fn get_two(
        &mut self,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

#[test]
fn duplicate_server_topics_are_rejected_before_startup() {
    let err = DuplicateServerTopicTest::__validate_server_topics().unwrap_err();
    assert!(err.contains("duplicate server topic"));
    assert!(err.contains("asset/get"));
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "map-test", api = y2026_1)]
struct MapTest {
    grid: Arc<Vec<u8>>,
}

struct MapTestState {
    grid: Arc<Vec<u8>>,
}

#[phoxal::runtime]
impl MapTest {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            grid: Arc::new(vec![0; 4]),
        })
    }

    #[server_snapshot(topic = api::topic::new().map().submap())]
    async fn submap(
        state: Snapshot<MapTestState>,
        _request: api::map::SubmapRequest,
    ) -> ServerResult<api::map::SubmapResponse> {
        Ok(api::map::SubmapResponse {
            width: 2,
            height: 2,
            resolution_m: 0.1,
            cells: state.grid.as_ref().clone(),
        })
    }

    #[snapshot]
    fn snapshot(&self) -> MapTestState {
        MapTestState {
            grid: Arc::clone(&self.grid),
        }
    }
}

#[tokio::test]
async fn snapshot_server_dispatch_reads_committed_state() {
    let rt = MapTest {
        grid: Arc::new(vec![1, 2, 3, 4]),
    };
    assert!(MapTest::HAS_SNAPSHOT);
    assert_eq!(MapTest::__snapshot_server_topics(), &["map/submap"]);

    let snapshot = Arc::new(rt.__take_snapshot());
    let request = MessagePack::encode(&api::map::SubmapRequest {
        min_x_m: 0.0,
        min_y_m: 0.0,
        max_x_m: 1.0,
        max_y_m: 1.0,
    })
    .unwrap();

    let api = <<api::map::SubmapRequest as ContractBody>::Api as ApiVersion>::ID.to_string();
    let family = <api::map::SubmapRequest as ContractBody>::FAMILY.to_string();
    let reply = MapTest::__serve_snapshot(snapshot, "map/submap".to_string(), api, family, request)
        .await
        .unwrap();
    let response: api::map::SubmapResponse = MessagePack::decode(&reply.payload).unwrap();
    assert_eq!(response.cells, vec![1, 2, 3, 4]);
}
