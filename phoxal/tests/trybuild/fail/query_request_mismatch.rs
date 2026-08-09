use phoxal::api;
use phoxal_supervisor_api::supervisor;
use phoxal::prelude::*;

struct Api;

#[phoxal::service(api = Api)]
struct BadRequest;

impl Participant for BadRequest {
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

impl BadRequest {
    fn get(
        &self,
        _api: &Api,
        _query: QueryContext,
        _request: api::map::SubmapRequest,
        _state: &mut (),
    ) -> QueryResult<supervisor::asset::GetResponse> {
        Ok(supervisor::asset::GetResponse::Missing)
    }
}

fn main() {}
