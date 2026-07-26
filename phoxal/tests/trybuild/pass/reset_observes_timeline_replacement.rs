#![deny(warnings)]

use phoxal::api;
use phoxal::prelude::*;

#[derive(phoxal::Api)]
struct Api {
    manual: Subscriber<api::motion::ManualCommand>,
}

#[phoxal::service(id = "reset-observer", config = (), api = Api)]
struct Resettable;

#[phoxal::behavior]
impl Resettable {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let cap = ctx.owner_capability();
        Ok((
            Self,
            Self::Api {
                manual: ctx
                    .subscriber(api::topic::internal::new(cap).motion().manual(), 8)
                    .await?,
            },
        ))
    }

    #[reset]
    async fn reset(&mut self, api: &mut Self::Api, ctx: ResetContext) -> Result<()> {
        let _ = (
            api.manual.try_recv(),
            ctx.previous_timeline(),
            ctx.new_timeline(),
        );
        Ok(())
    }
}

fn main() {}
