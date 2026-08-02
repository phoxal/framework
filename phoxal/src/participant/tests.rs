//! Direct tests for typed query registration and serialized state mutation.

use crate::api;
use crate::bus::{Codec, MessagePack, QueryCode, QueryFailure};
use crate::participant::api::QueryRegistration;
use crate::prelude::*;

struct Api;

#[derive(Default)]
struct QueryState {
    calls: Vec<String>,
}

#[phoxal::service(id = "query-test", state = QueryState, api = Api)]
struct QueryParticipant;

impl Participant for QueryParticipant {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok((QueryState::default(), Api))
    }
}

impl QueryParticipant {
    async fn get(
        &self,
        _api: &Api,
        request: api::supervisor::asset::GetRequest,
        state: &mut QueryState,
    ) -> QueryResult<api::supervisor::asset::GetResponse> {
        state.calls.push(request.path.clone());
        if request.path == "ok" {
            Ok(api::supervisor::asset::GetResponse::Found {
                bytes: vec![1, 2, 3],
            })
        } else {
            Err(QueryFailure::not_found("no such asset"))
        }
    }
}

#[tokio::test]
async fn typed_query_dispatch_decodes_mutates_and_encodes() {
    let registration = QueryRegistration::new(
        "v0.1/supervisor/asset/get".to_string(),
        QueryParticipant::get,
    );
    let participant = QueryParticipant;
    let api = Api;
    let mut state = QueryState::default();

    let first = MessagePack::encode(&api::supervisor::asset::GetRequest {
        path: "ok".to_string(),
    })
    .unwrap();
    let reply = registration
        .dispatch(&participant, &api, &mut state, first)
        .await
        .unwrap();
    let response: api::supervisor::asset::GetResponse =
        MessagePack::decode(&reply.payload).unwrap();
    assert!(matches!(
        response,
        api::supervisor::asset::GetResponse::Found { .. }
    ));

    let second = MessagePack::encode(&api::supervisor::asset::GetRequest {
        path: "missing".to_string(),
    })
    .unwrap();
    let failure = registration
        .dispatch(&participant, &api, &mut state, second)
        .await
        .unwrap_err();
    assert_eq!(failure.code, QueryCode::NotFound);
    assert_eq!(state.calls, ["ok", "missing"]);
}
