// #[setup] must return `Result<(Self, Self::Api)>` (D22).
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::service(id = "setup-bad-return", api = ())]
struct SetupBadReturn;

#[phoxal::behavior]
impl SetupBadReturn {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Self {
        Self
    }
}

fn main() {}
