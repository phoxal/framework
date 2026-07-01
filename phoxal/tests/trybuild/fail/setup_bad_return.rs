// #[setup] must return `Result<Self>` (D22).
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "setup-bad-return", api = y2026_1)]
struct SetupBadReturn {}

#[phoxal::runtime]
impl SetupBadReturn {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Self {
        Self {}
    }
}

fn main() {}
