// Plan #07: privileged tools use the explicit raw namespace for permissive bus
// access. The import is intentionally grepable.
use phoxal::prelude::*;

#[derive(phoxal::Tool)]
#[phoxal(id = "raw-tool", api = y2026_1)]
struct RawTool {}

#[phoxal::behavior]
impl RawTool {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        let _config = phoxal::raw::BusConfig::in_process("dev", "robot");
        let _open = phoxal::raw::Bus::open;
        Ok(Self {})
    }
}

fn main() {}
