use phoxal::api;
use phoxal::prelude::*;

pub struct Api;

#[phoxal::service(state = (), api = Api)]
pub struct MyService;

impl Participant for MyService {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let _ = api::topic::client();
        Ok(((), Api))
    }
}

fn main() {}
