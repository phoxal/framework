use phoxal::prelude::*;

struct State;

#[derive(phoxal::Service)]
#[phoxal(id = "snapshot-mut-self", api = y2026_1)]
struct SnapshotMutSelf {}

#[phoxal::behavior]
impl SnapshotMutSelf {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[snapshot]
    fn snapshot(&mut self) -> State {
        State
    }
}

fn main() {}
