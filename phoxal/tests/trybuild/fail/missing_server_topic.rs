// `#[server(...)]` requires `api = <Api struct field name>` naming the declared
// `Server<Req, Resp>` slot this handler implements.
use phoxal_api::v1 as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    get: Server<api::asset::GetRequest, api::asset::GetResponse>,
}

#[phoxal::service(id = "missing-server-topic")]
struct MissingServerTopic;

#[phoxal::behavior]
impl MissingServerTopic {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                get: ctx.server(api::topic::new().asset().get()).await?,
            },
        ))
    }

    #[server]
    async fn get(
        &mut self,
        _api: &mut Self::Api,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        unimplemented!()
    }
}

fn main() {}
