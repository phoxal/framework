// The world clock is a sibling semantic of `State`, never a subtype of it, so
// the ordinary state publisher every participant has cannot mint a world step.
// Both halves of the refusal are pinned: the participant builder (which also
// refuses the runtime family) and the handle itself, where the semantic is the
// only bound in play.
use phoxal::prelude::*;
use phoxal::runtime::api as runtime;

#[phoxal::service(id = "world-clock-minter")]
struct WorldClockMinter;

impl Participant for WorldClockMinter {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let _publisher = ctx.state_publisher(runtime::topics().simulation().clock().owner())?;
        Ok(((), ()))
    }
}

fn state_publisher_over_the_clock(bus: phoxal::bus::BusHandle) {
    let _ = phoxal::bus::StatePublisher::new(
        bus,
        &runtime::topics().simulation().clock().owner(),
    );
}

fn main() {}
