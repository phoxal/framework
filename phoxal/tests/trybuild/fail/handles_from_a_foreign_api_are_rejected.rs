// A participant declares one API (`ParticipantSpec::ContractApi`, fixed by the
// role attribute to the train-selected facade), and every handle it builds must
// come from that API. A body bound to any other `ApiVersion` - a second
// revision, or another `phoxal_api_tree!` tree such as a process-boundary
// protocol - is rejected at the builder call, which is what makes the `api`
// field of the participant's embedded metadata record a checked statement.
use phoxal::bus::{
    ApiVersion, EndpointDescriptor, EndpointKind, Publish, StateContract,
    StateDeliveryContract, Topic,
};
use phoxal::prelude::*;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ForeignState;

enum ForeignApi {}

impl ApiVersion for ForeignApi {
    const ID: &'static str = "foreign";
}

struct ForeignStateEndpoint;

impl EndpointDescriptor for ForeignStateEndpoint {
    type Api = ForeignApi;
    type Payload = ForeignState;
    const NAME: &'static str = "foreign::drive::state";
    const VERSION: &'static str = "foreign";
    const CONTRACT: &'static str = "drive/state";
    const TOPIC: &'static str = "foreign/drive/state";
    const KIND: EndpointKind = EndpointKind::State;
}

impl StateContract for ForeignStateEndpoint {}
impl StateDeliveryContract for ForeignStateEndpoint {}

#[phoxal::service(id = "mixed-api")]
struct MixedApi;

impl Participant for MixedApi {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let topic: Topic<Publish<ForeignStateEndpoint>> =
            Topic::new_static("foreign/drive/state");
        let _publisher = ctx.state_publisher(topic)?;
        Ok(((), ()))
    }
}

fn main() {}
