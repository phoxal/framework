// A tool joins the *execution*, not the clock (#952 section B/D). It gets a raw
// bus, so it can observe and command - but it cannot obtain a `RobotInstant`
// and it cannot publish checked state, because that needs a step token only the
// runner or the world authority can mint.
//
// Three independent proofs in one fixture:
//
// 1. there is no clock accessor on a tool's `SetupContext`;
// 2. `StepStamp` is sealed, so a tool cannot invent a step stamp of its own;
// 3. `StatePublisher::publish` therefore has no argument a tool can supply.

use phoxal::api;
use phoxal::prelude::*;

struct ForgedStep;

// (2) The step-stamp trait is sealed: no downstream type can implement it.
impl phoxal::bus::StepStamp for ForgedStep {
    fn instant(&self) -> phoxal::bus::RobotInstant {
        unimplemented!()
    }
}

#[phoxal::tool(id = "time-forging-tool")]
struct TimeForgingTool;

#[phoxal::behavior]
impl TimeForgingTool {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // (1) A tool has no clock to read.
        let _now = ctx.clock();

        // (3) Publishing checked state needs a stamp the tool cannot produce.
        let state = phoxal::raw::StatePublisher::<api::drive::State>::new(
            ctx.raw_bus(),
            &api::topic::internal::new(ctx.owner_capability())
                .drive()
                .state(),
        )?;
        state.publish(&ForgedStep, unimplemented!())?;
        Ok((Self, ()))
    }
}

fn main() {}
