// A participant declares one API (`ParticipantSpec::ContractApi`, fixed by the
// role attribute to the train-selected facade), and every handle it builds must
// come from that API. A body bound to any other `ApiVersion` - a second
// revision, or another `phoxal_api_tree!` tree such as a process-boundary
// protocol - is rejected at the builder call, which is what makes the `api`
// field of the participant's embedded metadata record a checked statement.
use phoxal::bus::{
    ApiVersion, ContractBody, DeliveryFamily, Publish, StateContract, Topic, TopicRole,
};
use phoxal::prelude::*;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ForeignState;

enum ForeignApi {}

impl ApiVersion for ForeignApi {
    const ID: &'static str = "foreign";
}

impl ContractBody for ForeignState {
    type Api = ForeignApi;
    const NAME: &'static str = "foreign::drive::State";
    const VERSION: &'static str = "foreign";
    const CONTRACT: &'static str = "drive::State";
    const TOPIC: &'static str = "foreign/drive/state";
    const ROLE: TopicRole = TopicRole::State;
    const DELIVERY: DeliveryFamily = DeliveryFamily::State;
}

impl StateContract for ForeignState {}

#[phoxal::service(id = "mixed-api")]
struct MixedApi;

impl Participant for MixedApi {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let topic: Topic<Publish<ForeignState>> = Topic::new_static("foreign/drive/state");
        let _publisher = ctx.state_publisher(topic)?;
        Ok(((), ()))
    }
}

fn main() {}
