use phoxal::prelude::*;

#[phoxal::service(id = "duplicate-reset", config = (), api = ())]
struct DuplicateReset;

#[phoxal::behavior]
impl DuplicateReset {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[reset]
    async fn reset(&mut self, _ctx: ResetContext) -> Result<()> {
        Ok(())
    }

    #[reset]
    async fn reset(&mut self, _ctx: ResetContext) -> Result<()> {
        Ok(())
    }
}

fn main() {}
