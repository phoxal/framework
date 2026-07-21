// Plan #15: a tool is a thin raw-bus runner, not a typed-graph participant, so
// `#[server]` (the exclusive query-server surface) is not allowed on it.
use phoxal::api as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    get: Server<api::asset::GetRequest, api::asset::GetResponse>,
}

#[phoxal::tool(id = "tool-forbids-server", api = Api)]
struct ToolForbidsServer;

#[phoxal::behavior]
impl ToolForbidsServer {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                get: ctx.server(api::topic::new().asset().get()).await?,
            },
        ))
    }

    #[server(api = get)]
    async fn get(
        &mut self,
        _api: &mut Self::Api,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

fn main() {}
