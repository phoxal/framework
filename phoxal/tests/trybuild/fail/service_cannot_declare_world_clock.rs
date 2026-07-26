// organization#957 leftover, closed: "only a simulator can mint world steps"
// used to be a doc comment nothing enforced - any participant could declare a
// `StatePublisher<api::simulation::Clock>` field and build it through the
// ordinary `ctx.state_publisher(...)` builder every participant kind has.
// `simulation::Clock` is now declared `world_clock` in the api tree, which
// implements the disjoint `WorldClockContract` instead of `StateContract`, so
// an ordinary `#[phoxal::service]` cannot even name this field: `StateContract`
// is not implemented for it.

use phoxal::api;
use phoxal::prelude::*;

#[derive(phoxal::Api)]
struct Api {
    clock: StatePublisher<api::simulation::Clock>,
}

#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::service(id = "rogue-clock-service", config = Config, api = Api)]
struct RogueClockService;

#[phoxal::behavior]
impl RogueClockService {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // ERROR: `api::simulation::Clock` does not implement `StateContract`
        // (it implements `WorldClockContract`, which only
        // `SetupContextSimulatorExt::world_clock_publisher` accepts, and that
        // trait is unavailable here - `RogueClockService` is a
        // `#[phoxal::service]`, not a `#[phoxal::simulator]`).
        let clock = ctx
            .state_publisher(api::topic::owner().simulation().clock())
            .await?;
        Ok((Self, Api { clock }))
    }
}

fn main() {}
