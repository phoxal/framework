// Dynamic handles are allowed, but their family must still be declared.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "undeclared-dynamic", api = y2026_1)]
struct UndeclaredDynamic {}

#[phoxal::runtime]
impl UndeclaredDynamic {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let _motor = ctx
            .publisher(api::topic::new().component("base").motor("left").command())
            .await?;
        Ok(Self {})
    }
}

fn main() {}
