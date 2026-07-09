use phoxal::prelude::*;

struct State;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::service(id = "snapshot-mut-self", api = ())]
struct SnapshotMutSelf;

#[phoxal::behavior]
impl SnapshotMutSelf {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[snapshot]
    fn snapshot(&mut self) -> State {
        State
    }
}

fn main() {}
