// An endpoint's semantic decides which operations apply to it. `drive/target`
// is a setpoint - newest-actionable intent, no ordered history - so receiving
// it as an ordered event stream is refused at the builder call.
use phoxal::api;
use phoxal::prelude::*;

#[phoxal::service(id = "setpoint-as-event")]
struct SetpointAsEvent;

impl Participant for SetpointAsEvent {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let _events = ctx.event_receiver(api::topics().drive().target().owner()).await?;
        Ok(((), ()))
    }
}

fn main() {}
