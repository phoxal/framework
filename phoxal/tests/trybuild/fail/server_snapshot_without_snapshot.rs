// #[server_snapshot] requires a #[snapshot] provider on the same runtime.
use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

struct State;

#[derive(phoxal::Runtime)]
#[phoxal(id = "no-snap", api = y2026_1)]
struct NoSnap {
    revision: Publisher<api::map::Revision>,
}

#[phoxal::runtime]
impl NoSnap {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            revision: ctx
                .publisher(api::topic::internal::new().map().revision())
                .await?,
        })
    }

    #[server_snapshot(topic = api::topic::internal::new().map().submap())]
    async fn submap(
        _state: Snapshot<State>,
        _request: api::map::SubmapRequest,
    ) -> ServerResult<api::map::SubmapResponse> {
        unimplemented!()
    }
}

fn main() {}
