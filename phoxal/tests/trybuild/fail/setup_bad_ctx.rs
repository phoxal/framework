// #[setup] first argument must be `ctx: &mut SetupContext<Self>`.
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "setup-bad-ctx", api = y2026_1)]
struct SetupBadCtx {}

#[phoxal::runtime]
impl SetupBadCtx {
    #[setup]
    async fn setup(_ctx: &mut StepContext) -> Result<Self> {
        Ok(Self {})
    }
}

fn main() {}
