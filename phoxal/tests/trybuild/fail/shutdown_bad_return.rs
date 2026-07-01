// #[shutdown] must return `Result<()>`.
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "shutdown-bad-return", api = y2026_1)]
struct ShutdownBadReturn {}

#[phoxal::runtime]
impl ShutdownBadReturn {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[shutdown]
    async fn shutdown(&mut self) {}
}

fn main() {}
