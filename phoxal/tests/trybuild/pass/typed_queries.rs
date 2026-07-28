use phoxal::api;
use phoxal::prelude::*;

struct Api;
struct State(u64);

#[phoxal::service(id = "typed-queries", state = State, api = Api)]
struct TypedQueries;

impl Participant for TypedQueries {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        ctx.query(api::topic::owner().asset().get(), Self::get)
            .await?;
        ctx.query(api::topic::owner().map().submap(), Self::submap)
            .await?;
        Ok((State(0), Api))
    }
}

impl TypedQueries {
    async fn get(
        &self,
        _api: &Api,
        _request: api::asset::GetRequest,
        state: &mut State,
    ) -> QueryResult<api::asset::GetResponse> {
        state.0 += 1;
        Ok(api::asset::GetResponse::Missing)
    }

    async fn submap(
        &self,
        _api: &Api,
        _request: api::map::SubmapRequest,
        _state: &mut State,
    ) -> QueryResult<api::map::SubmapResponse> {
        Ok(api::map::SubmapResponse {
            width: 0,
            height: 0,
            resolution_m: 0.1,
            cells: Vec::new(),
        })
    }
}

fn main() {}
