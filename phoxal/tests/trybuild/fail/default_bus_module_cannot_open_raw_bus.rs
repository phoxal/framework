// Plan #07: the default checked-participant bus module exposes typed contract
// vocabulary, not raw session-opening APIs. Raw access is a conscious opt-in at
// `phoxal::raw`.
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "default-raw-bus-open", api = y2026_1)]
struct DefaultRawBusOpen {}

#[phoxal::behavior]
impl DefaultRawBusOpen {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        let _config = phoxal::bus::BusConfig::in_process("dev", "robot");
        let _open = phoxal::bus::Bus::open;
        let _run_with_bus = phoxal::participant::run_with_bus::<DefaultRawBusOpen, _, _>;
        Ok(Self {})
    }
}

fn main() {}
