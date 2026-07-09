// A handle body whose ContractBody::Api is not the participant's selected API is a
// compile error (D60).
use phoxal::prelude::*;

enum ForeignApi {}

impl phoxal_api::ApiVersion for ForeignApi {
    const ID: &'static str = "foreign";
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ForeignBody;

impl phoxal_api::ContractBody for ForeignBody {
    type Api = ForeignApi;
    const TOPIC: &'static str = "foreign/body";
}

#[derive(phoxal::Service)]
#[phoxal(id = "wrong-api", api = y2026_1)]
struct WrongApi {
    body: Publisher<ForeignBody>,
}

#[phoxal::behavior]
impl WrongApi {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        unimplemented!()
    }
}

fn main() {}
