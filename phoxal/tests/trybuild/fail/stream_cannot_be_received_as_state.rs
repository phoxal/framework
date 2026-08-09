use phoxal::api;
use phoxal::prelude::*;

#[phoxal::service(id = "wrong-stream-receiver")]
struct WrongStreamReceiver;

impl Participant for WrongStreamReceiver {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let _result = ctx
            .state_view(api::topic::client().navigation().result())
            .await?;
        Ok(((), ()))
    }
}

fn main() {}
