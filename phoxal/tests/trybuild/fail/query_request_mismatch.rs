use phoxal::api;
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
        ctx.query(api::topic::owner().asset().get(), Self::get)
            .await?;
        Ok(((), Api))
    }
}

impl BadRequest {
    async fn get(
        &self,
        _api: &Api,
        _request: api::map::SubmapRequest,
        _state: &mut (),
    ) -> QueryResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

fn main() {}
