use phoxal::api;
use phoxal::prelude::*;

#[phoxal::service(id = "wrong-state-receiver")]
struct WrongStateReceiver;

impl Participant for WrongStateReceiver {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let _state = ctx
            .stream_receiver(api::topic::client().drive().state())
            .await?;
        Ok(((), ()))
    }
}

fn main() {}
