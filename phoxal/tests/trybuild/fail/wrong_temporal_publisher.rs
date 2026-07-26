// The robot time a publisher can express is fixed by the contract's temporal
// role (#952 section D). Reaching for the wrong publisher is a compile error,
// not a review question: a `command` contract has no `StateContract` impl, and
// a `state` contract has no `CommandContract` impl.

use phoxal::api;
use phoxal::prelude::*;

#[derive(phoxal::Api)]
struct Api {
    // `drive/target` is declared `command`: it expresses no robot time, so it
    // cannot be published at a step.
    target: StatePublisher<api::drive::Target>,
    // `drive/state` is declared `state`: it is published at a step, so it
    // cannot be sent as a timeless command.
    state: CommandPublisher<api::drive::State>,
    // `component/*/imu/*/sample` is declared `measurement`: it carries a
    // capture stamp, not a step token.
    imu: StatePublisher<api::component::imu::Sample>,
}

#[phoxal::service(id = "wrong-temporal-publisher", config = (), api = Api)]
struct WrongTemporalPublisher;

#[phoxal::behavior]
impl WrongTemporalPublisher {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        unimplemented!()
    }
}

fn main() {}
