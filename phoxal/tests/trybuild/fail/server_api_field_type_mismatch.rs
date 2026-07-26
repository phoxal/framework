// `#[server(api = field)]`'s `field` must name an `Api` struct field typed
// `Server<Req, Resp>` matching the handler's own request/reply types. Here
// `get` is declared as `Server<asset::GetRequest, asset::GetResponse>`, but
// the `#[server(api = get)]` handler answers `map::SubmapRequest` /
// `map::SubmapResponse` - a type mismatch the generated field-cross-check
// must catch at compile time.
use phoxal::api as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    get: Server<api::asset::GetRequest, api::asset::GetResponse>,
}

#[phoxal::service(id = "server-field-type-mismatch")]
struct ServerFieldTypeMismatch;

#[phoxal::behavior]
impl ServerFieldTypeMismatch {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                get: ctx.server(api::topic::client().asset().get()).await?,
            },
        ))
    }

    // ERROR: `get` is `Server<asset::GetRequest, asset::GetResponse>`, not
    // `Server<map::SubmapRequest, map::SubmapResponse>`.
    #[server(api = get)]
    async fn submap(
        &mut self,
        _api: &mut Self::Api,
        _request: api::map::SubmapRequest,
    ) -> ServerResult<api::map::SubmapResponse> {
        unimplemented!()
    }
}

fn main() {}
