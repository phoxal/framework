use phoxal::api;
use phoxal::supervisor::api as supervisor;
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
        ctx.query(supervisor::topic::owner().bundle().get(), Self::get)
            ?;
        ctx.query(api::topic::owner().map().submap(), Self::submap)
            ?;
        Ok((State(0), Api))
    }
}

impl TypedQueries {
    fn get(
        &self,
        _api: &Api,
        _query: QueryContext,
        _request: supervisor::bundle::GetRequest,
        state: &mut State,
    ) -> QueryResult<supervisor::bundle::GetResponse> {
        state.0 += 1;
        Ok(supervisor::bundle::GetResponse::Missing)
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
