// Two robot-family queries, because the endpoint is what fixes a handler's
// request and response types. The host families are a different profile's
// surface and a participant never serves one.
use phoxal::api;
use phoxal::prelude::*;

struct Api;
struct State(u64);

#[phoxal::service(id = "typed-queries", state = State, api = Api)]
struct TypedQueries;

impl Participant for TypedQueries {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        ctx.query(api::topics().navigation().cancel().owner(), Self::cancel)?;
        ctx.query(api::topics().map().submap().owner(), Self::submap)?;
        Ok((State(0), Api))
    }
}

impl TypedQueries {
    fn cancel(
        &self,
        _api: &Api,
        _query: QueryContext,
        _request: api::navigation::CancelRequest,
        state: &mut State,
    ) -> QueryResult<api::navigation::CancelResponse> {
        state.0 += 1;
        Ok(api::navigation::CancelResponse::Accepted)
    }

    fn submap(
        &self,
        _api: &Api,
        _query: QueryContext,
        _request: api::map::SubmapRequest,
        _state: &mut State,
    ) -> QueryResult<api::map::SubmapResponse> {
        let bounds = api::map::Bounds {
            min_x_m: 0.0,
            min_y_m: 0.0,
            max_x_m: 0.1,
            max_y_m: 0.1,
        };
        Ok(api::map::SubmapResponse::Window(api::map::GridWindow {
            frame_id: "map".to_owned(),
            origin_pose: api::map::Pose {
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
            },
            cell_origin: api::map::Point { x_m: 0.0, y_m: 0.0 },
            width: 1,
            height: 1,
            resolution_m: 0.1,
            cells: vec![api::map::Occupancy::Free],
            revision: 0,
            requested: bounds.clone(),
            covered: bounds,
        }))
    }
}

fn main() {}
