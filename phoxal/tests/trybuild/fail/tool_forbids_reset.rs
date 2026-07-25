use phoxal::prelude::*;

#[phoxal::tool(id = "tool-with-reset")]
struct ToolWithReset;

#[phoxal::behavior]
impl ToolWithReset {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[reset]
    async fn reset(&mut self, _ctx: ResetContext) -> Result<()> {
        Ok(())
    }
}

fn main() {}
