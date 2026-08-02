use phoxal::api;
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
        ctx.query(api::topic::owner().supervisor().asset().get(), Self::get)
            .await?;
        Ok(((), Api))
    }
}

impl BadResponse {
    async fn get(
        &self,
        _api: &Api,
        _request: api::supervisor::asset::GetRequest,
        _state: &mut (),
    ) -> QueryResult<api::map::SubmapResponse> {
        unreachable!()
    }
}

fn main() {}
