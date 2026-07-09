// #[shutdown] must return `Result<()>`.
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::service(id = "shutdown-bad-return", api = ())]
struct ShutdownBadReturn;

#[phoxal::behavior]
impl ShutdownBadReturn {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[shutdown]
    async fn shutdown(&mut self, _api: &mut Self::Api) {}
}

fn main() {}
