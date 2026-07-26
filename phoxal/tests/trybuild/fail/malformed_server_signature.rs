// Server handlers must return ServerResult<Resp>.
use phoxal::api as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    get: Server<api::asset::GetRequest, api::asset::GetResponse>,
}

#[phoxal::service(id = "bad-server")]
struct BadServer;

#[phoxal::behavior]
impl BadServer {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                get: ctx.server(api::topic::client().asset().get()).await?,
            },
        ))
    }

    #[server(api = get)]
    async fn get(
        &mut self,
        _api: &mut Self::Api,
        _request: api::asset::GetRequest,
    ) -> Result<api::asset::GetResponse> {
        unimplemented!()
    }
}

fn main() {}
