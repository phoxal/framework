// A participant declares one contract family (`ParticipantSpec::ContractApi`,
// fixed by the role attribute to the authoring facade), and every handle it
// builds must come from that family. A body bound to any other `ApiFamily` -
// another family, or a semantic protocol tree such as a process-boundary
// protocol - is rejected at the builder call.
use phoxal::bus::{
    ApiFamily, EndpointDescriptor, EndpointKind, Publish, StateContract, StateDeliveryContract,
    Topic,
};
use phoxal::prelude::*;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ForeignState;

enum ForeignApi {}

impl ApiFamily for ForeignApi {
    const ID: &'static str = "foreign";
}

struct ForeignStateEndpoint;

impl EndpointDescriptor for ForeignStateEndpoint {
    type Api = ForeignApi;
    type Payload = ForeignState;
    const NAME: &'static str = "foreign::drive::state";
    const FAMILY: &'static str = "foreign";
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
        let topic: Topic<Publish<ForeignStateEndpoint>> = Topic::new_static("foreign/drive/state");
        let _publisher = ctx.state_publisher(topic)?;
        Ok(((), ()))
    }
}

fn main() {}
