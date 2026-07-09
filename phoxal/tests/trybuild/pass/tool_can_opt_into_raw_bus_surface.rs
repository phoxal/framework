// Plan #07: privileged tools use the explicit raw namespace for permissive bus
// access. The import is intentionally grepable.
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::tool(id = "raw-tool")]
struct RawTool;

#[phoxal::behavior]
impl RawTool {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let _config = phoxal::raw::BusConfig::in_process("dev", "robot");
        let _open = phoxal::raw::Bus::open;
        Ok((Self, ()))
    }
}

fn main() {}
