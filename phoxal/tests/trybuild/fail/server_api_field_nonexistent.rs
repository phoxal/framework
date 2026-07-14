// `#[server(api = field)]`'s `field` must name an actual `Api` struct field.
// `get` here names no field on `Api` (the struct has none at all), so the
// generated field-cross-check must fail to compile with rustc's own
// "no field" error, actionable at the `api = get` site.
use phoxal_api::v1 as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {}

#[phoxal::service(id = "server-field-nonexistent")]
struct ServerFieldNonexistent;

#[phoxal::behavior]
impl ServerFieldNonexistent {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, Self::Api {}))
    }

    // ERROR: `Api` has no field named `get`.
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
