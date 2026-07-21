//! Macro server-dispatch tests: the generated `__serve_exclusive` /
//! `__serve_snapshot` decode the request, call the handler, and encode the reply
//! (or a `QueryFailure`). These exercise the macro output directly, without a bus.
#![allow(clippy::assertions_on_constants)]

use std::sync::Arc;

use crate::api;
use crate::bus::{Codec, ContractBody, MessagePack, QueryCode};
use crate::participant::ParticipantLifecycle;
use crate::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct AssetTestApi {
    get: Server<api::asset::GetRequest, api::asset::GetResponse>,
}

#[phoxal::service(id = "asset-test", api = AssetTestApi)]
struct AssetTest {
    present: bool,
}

#[phoxal::behavior]
impl AssetTest {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self, Self::Api)> {
        Ok((
            Self { present: true },
            Self::Api {
                get: ctx.server(api::topic::new().asset().get()).await?,
            },
        ))
    }

    #[server(api = get)]
    async fn get(
        &mut self,
        _api: &mut Self::Api,
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
    let mut api = AssetTestApi { get: Server::new() };

    // The declared topic shows up in the metadata, version-qualified (D1).
    assert_eq!(AssetTest::__exclusive_server_topics(), &["v0.1/asset/get"]);
    assert!(AssetTest::__snapshot_server_topics().is_empty());
    AssetTest::__validate_server_topics().unwrap();
    assert!(!AssetTest::HAS_SNAPSHOT);
    assert!(
        AssetTest::SERVER_CONTRACTS
            .iter()
            .any(|c| c.topic == <api::asset::GetRequest as ContractBody>::TOPIC)
    );

    let request = MessagePack::encode(&api::asset::GetRequest {
        path: "ok".to_string(),
    })
    .unwrap();
    let reply = rt
        .__serve_exclusive(&mut api, "v0.1/asset/get", &request)
        .await
        .unwrap();
    let response: api::asset::GetResponse = MessagePack::decode(&reply.payload).unwrap();
    assert!(matches!(response, api::asset::GetResponse::Found { .. }));

    let request = MessagePack::encode(&api::asset::GetRequest {
        path: "missing".to_string(),
    })
    .unwrap();
    let failure = rt
        .__serve_exclusive(&mut api, "v0.1/asset/get", &request)
        .await
        .unwrap_err();
    assert_eq!(failure.code, QueryCode::NotFound);

    // A request on a topic this participant does not serve is unimplemented -
    // correctness now comes from the key (D1), so there is no separate
    // schema/family mismatch to test.
    let failure = rt
        .__serve_exclusive(&mut api, "v0.1/other/topic", &request)
        .await
        .unwrap_err();
    assert_eq!(failure.code, QueryCode::Unimplemented);
}

#[derive(phoxal::Api)]
struct DuplicateServerTopicTestApi {
    get_one: Server<api::asset::GetRequest, api::asset::GetResponse>,
    get_two: Server<api::asset::GetRequest, api::asset::GetResponse>,
}

#[phoxal::service(id = "duplicate-server-topic-test", api = DuplicateServerTopicTestApi)]
struct DuplicateServerTopicTest {}

#[phoxal::behavior]
impl DuplicateServerTopicTest {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self, Self::Api)> {
        Ok((
            Self {},
            Self::Api {
                get_one: ctx.server(api::topic::new().asset().get()).await?,
                get_two: ctx.server(api::topic::new().asset().get()).await?,
            },
        ))
    }

    #[server(api = get_one)]
    async fn get_one(
        &mut self,
        _api: &mut Self::Api,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }

    #[server(api = get_two)]
    async fn get_two(
        &mut self,
        _api: &mut Self::Api,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

#[test]
fn duplicate_server_topics_are_rejected_before_startup() {
    let err = DuplicateServerTopicTest::__validate_server_topics().unwrap_err();
    assert!(err.contains("duplicate server topic"));
    assert!(err.contains("v0.1/asset/get"));
}

#[derive(phoxal::Api)]
struct MapTestApi {
    submap: Server<api::map::SubmapRequest, api::map::SubmapResponse>,
}

#[phoxal::service(id = "map-test", api = MapTestApi)]
struct MapTest {
    grid: Arc<Vec<u8>>,
}

struct MapTestState {
    grid: Arc<Vec<u8>>,
}

#[phoxal::behavior]
impl MapTest {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self, Self::Api)> {
        Ok((
            Self {
                grid: Arc::new(vec![0; 4]),
            },
            Self::Api {
                submap: ctx.server(api::topic::new().map().submap()).await?,
            },
        ))
    }

    #[server_snapshot(api = submap)]
    async fn submap(
        state: Snapshot<MapTestState>,
        _api: &Self::Api,
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
    let api = Arc::new(MapTestApi {
        submap: Server::new(),
    });
    assert!(MapTest::HAS_SNAPSHOT);
    assert_eq!(MapTest::__snapshot_server_topics(), &["v0.1/map/submap"]);

    let snapshot = Arc::new(rt.__take_snapshot());
    let request = MessagePack::encode(&api::map::SubmapRequest {
        min_x_m: 0.0,
        min_y_m: 0.0,
        max_x_m: 1.0,
        max_y_m: 1.0,
    })
    .unwrap();

    let reply = MapTest::__serve_snapshot(snapshot, api, "v0.1/map/submap".to_string(), request)
        .await
        .unwrap();
    let response: api::map::SubmapResponse = MessagePack::decode(&reply.payload).unwrap();
    assert_eq!(response.cells, vec![1, 2, 3, 4]);
}
