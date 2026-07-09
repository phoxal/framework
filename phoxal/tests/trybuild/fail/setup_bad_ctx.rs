// #[setup] first argument must be `ctx: &mut SetupContext<Self>`.
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::service(id = "setup-bad-ctx", api = ())]
struct SetupBadCtx;

#[phoxal::behavior]
impl SetupBadCtx {
    #[setup]
    async fn setup(_ctx: &mut StepContext) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }
}

fn main() {}
