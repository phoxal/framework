// Participant IO accepts robot endpoints and nothing else. The supervisor
// family is a host-tooling surface, so a supervisor endpoint is refused at the
// builder call by the `RobotEndpoint` bound - not by a runtime mismatch, and
// not by a participant-local contract declaration.
use phoxal::prelude::*;
use phoxal::supervisor::api as supervisor;

#[phoxal::service(id = "foreign-family")]
struct ForeignFamily;

impl Participant for ForeignFamily {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let _publisher = ctx.state_publisher(supervisor::topics().snapshot().owner())?;
        Ok(((), ()))
    }
}

fn main() {}
