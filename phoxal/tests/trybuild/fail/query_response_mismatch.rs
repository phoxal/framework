use phoxal::api;
use phoxal_supervisor_api::supervisor;
use phoxal::prelude::*;

struct Api;

#[phoxal::service(api = Api)]
struct BadResponse;

impl Participant for BadResponse {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        ctx.query(supervisor::topic::owner().asset().get(), Self::get)
            ?;
        Ok(((), Api))
    }
}

impl BadResponse {
    fn get(
        &self,
        _api: &Api,
        _query: QueryContext,
        _request: supervisor::asset::GetRequest,
        _state: &mut (),
    ) -> QueryResult<api::map::SubmapResponse> {
        unreachable!()
    }
}

fn main() {}
