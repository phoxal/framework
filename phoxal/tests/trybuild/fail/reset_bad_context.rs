use phoxal::prelude::*;

#[phoxal::service(id = "reset-bad-context", config = (), api = ())]
struct InvalidReset;

#[phoxal::behavior]
impl InvalidReset {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[reset]
    async fn reset(&mut self, _ctx: StepContext) -> Result<()> {
        Ok(())
    }
}

fn main() {}
