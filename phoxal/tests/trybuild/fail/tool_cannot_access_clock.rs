use phoxal::prelude::*;

#[phoxal::tool(id = "clocked-tool")]
struct ClockedTool;

// Even manually adding the public graph marker cannot change the launch policy
// that #[phoxal::tool] fixed in the Participant impl.
impl phoxal::participant::TypedGraphSurface for ClockedTool {}

#[phoxal::behavior]
impl ClockedTool {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let _clock = ctx.clock();
        Ok((Self, ()))
    }
}

fn main() {
    let _inject = phoxal::raw::run_with_bus_clock::<
        ClockedTool,
        phoxal::participant::TestClock,
        std::future::Ready<()>,
    >;
}
