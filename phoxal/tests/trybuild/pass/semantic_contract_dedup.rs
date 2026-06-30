// Aliases and fully-qualified paths for the same body should not emit duplicate
// ContractUse metadata or conflicting Declares marker impls.
use phoxal_api::y2026_1;
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(
    id = "dedup",
    api = y2026_1,
    contracts(
        publishes(phoxal_api::y2026_1::drive::Target),
        publishes(y2026_1::drive::Target),
        subscribes(phoxal_api::y2026_1::drive::State),
        subscribes(y2026_1::drive::State),
    )
)]
struct Dedup {
    target: Publisher<api::drive::Target>,
    state: Subscriber<api::drive::State>,
}

#[phoxal::runtime]
impl Dedup {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            target: ctx.publisher(api::topic::new().drive().target()).await?,
            state: ctx
                .subscribe(api::topic::new().drive().state())
                .subscriber()
                .await?,
        })
    }

    #[step(hz = 1)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        self.target
            .publish_at(
                step.time(),
                api::drive::Target {
                    linear_x_mps: 0.0,
                    angular_z_radps: 0.0,
                    curvature_limit_radpm: None,
                },
            )
            .await?;
        let _ = self.state.try_recv();
        Ok(())
    }
}

fn main() {}
