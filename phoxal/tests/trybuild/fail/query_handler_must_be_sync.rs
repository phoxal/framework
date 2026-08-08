use phoxal::api;
use phoxal::prelude::*;

struct Api;

#[phoxal::service(id = "async-query-handler", api = Api)]
struct AsyncQueryHandler;

impl Participant for AsyncQueryHandler {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        ctx.query(api::topic::owner().supervisor().asset().get(), Self::get)?;
        Ok(((), Api))
    }
}

impl AsyncQueryHandler {
    async fn get(
        &self,
        _api: &Api,
        _query: QueryContext,
        _request: api::supervisor::asset::GetRequest,
        _state: &mut (),
    ) -> QueryResult<api::supervisor::asset::GetResponse> {
        Ok(api::supervisor::asset::GetResponse::Missing)
    }
}

fn main() {}
