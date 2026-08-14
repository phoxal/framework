use phoxal::api;
use phoxal_protocol::supervisor;
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
        ctx.query(supervisor::topic::owner().bundle().get(), Self::get)?;
        Ok(((), Api))
    }
}

impl AsyncQueryHandler {
    async fn get(
        &self,
        _api: &Api,
        _query: QueryContext,
        _request: supervisor::bundle::GetRequest,
        _state: &mut (),
    ) -> QueryResult<supervisor::bundle::GetResponse> {
        Ok(supervisor::bundle::GetResponse::Missing)
    }
}

fn main() {}
