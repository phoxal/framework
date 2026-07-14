// Exclusive query server fixture.
use phoxal_api::v1 as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    get: Server<api::asset::GetRequest, api::asset::GetResponse>,
}

#[phoxal::service(id = "asset-pass")]
struct AssetPass;

#[phoxal::behavior]
impl AssetPass {
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
