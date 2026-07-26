// #[server_snapshot] requires a #[snapshot] provider on the same participant.
use phoxal::api as api;
use phoxal::prelude::*;

struct State;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    revision: StatePublisher<api::map::Revision>,
    submap: Server<api::map::SubmapRequest, api::map::SubmapResponse>,
}

#[phoxal::service(id = "no-snap")]
struct NoSnap;

#[phoxal::behavior]
impl NoSnap {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                revision: ctx
                    .state_publisher(api::topic::owner().map().revision())
                    .await?,
                submap: ctx.server(api::topic::client().map().submap()).await?,
            },
        ))
    }

    #[server_snapshot(api = submap)]
    async fn submap(
        _state: Snapshot<State>,
        _request: api::map::SubmapRequest,
    ) -> ServerResult<api::map::SubmapResponse> {
        unimplemented!()
    }
}

fn main() {}
