// Wrong-API bodies cannot be smuggled in through explicit contracts(...).
use phoxal::prelude::*;

enum ForeignApi {}

impl phoxal_api::ApiVersion for ForeignApi {
    const ID: &'static str = "foreign";
}

macro_rules! foreign_body {
    ($name:ident, $family:literal, $topic:literal) => {
        #[derive(Clone, serde::Serialize, serde::Deserialize)]
        struct $name;

        impl phoxal_api::ContractBody for $name {
            type Api = ForeignApi;
            const FAMILY: &'static str = $family;
            const TOPIC: &'static str = $topic;
        }
    };
}

foreign_body!(ForeignPublish, "foreign::Publish", "foreign/publish");
foreign_body!(ForeignSubscribe, "foreign::Subscribe", "foreign/subscribe");
foreign_body!(ForeignRequest, "foreign::Request", "foreign/query");
foreign_body!(ForeignResponse, "foreign::Response", "foreign/query");

#[derive(phoxal::Service)]
#[phoxal(
    id = "wrong-api-explicit",
    api = y2026_1,
    contracts(
        publishes(ForeignPublish),
        subscribes(ForeignSubscribe),
        queries(ForeignRequest => ForeignResponse),
    )
)]
struct WrongApiExplicit {}

#[phoxal::runtime]
impl WrongApiExplicit {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }
}

fn main() {}
