//! Minimal compiled endpoint producer for the SDK's generated-client proof.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

phoxal::api!();
use phoxal::contracts::{Empty, Latest, RequestReply};
use phoxal::runtime::Context;

#[phoxal::message(package = "example.sdk_client.v0")]
pub struct ClientStatus {
    #[phoxal(tag = 1)]
    pub ready: bool,
}

#[phoxal::endpoints]
pub struct ClientApi {
    #[phoxal::output(projection = state, max_bytes = 256)]
    status: Latest<ClientStatus>,
    #[phoxal::operation(
        contract = "example.sdk_client.v0.InspectClient",
        max_items = 8,
        max_bytes = 256
    )]
    inspect: RequestReply<Empty, ClientStatus>,
}

pub struct Provider;
#[phoxal::runtime(contract = ClientApi, period_ms = 20)]
impl Provider {
    #[init]
    fn new(_: ()) -> phoxal::Result<Self> {
        Ok(Self)
    }
    #[handle(inspect)]
    fn inspect(
        &mut self,
        _ctx: &mut Context<'_, Self>,
        _request: Empty,
    ) -> phoxal::Result<ClientStatus> {
        Ok(ClientStatus { ready: true })
    }
    #[publish(status)]
    fn status(&self) -> ClientStatus {
        ClientStatus { ready: true }
    }
}
fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<Provider>()
}
