// Canonical handle fields must name ContractBody types.
use phoxal::prelude::*;

struct NotABody;

#[derive(phoxal::Runtime)]
#[phoxal(id = "not-a-body", api = y2026_1)]
struct NotBodyRuntime {
    target: Publisher<NotABody>,
}

#[phoxal::runtime]
impl NotBodyRuntime {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        unimplemented!()
    }
}

fn main() {}
