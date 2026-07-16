use phoxal::prelude::*;

#[derive(phoxal::Api)]
struct Api {}

#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::simulator(id = "clocked-simulator")]
struct ClockedSimulator;

#[phoxal::behavior]
impl ClockedSimulator {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, Api {}))
    }
}

fn main() {
    let _inject = phoxal::raw::run_with_bus_clock::<
        ClockedSimulator,
        phoxal::participant::TestClock,
        std::future::Ready<()>,
    >;
}
