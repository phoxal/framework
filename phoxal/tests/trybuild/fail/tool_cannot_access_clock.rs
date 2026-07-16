use phoxal::prelude::*;

#[phoxal::tool(id = "clocked-tool")]
struct ClockedTool;

#[phoxal::behavior]
impl ClockedTool {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let _clock = ctx.clock();
        Ok((Self, ()))
    }
}

fn main() {}
