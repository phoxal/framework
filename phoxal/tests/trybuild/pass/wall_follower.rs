// The authoring model's canonical fixture: `#[derive(phoxal::Api)]` +
// `#[derive(phoxal::Config)]` + `#[phoxal::service]` + `#[phoxal::behavior]`
// with a `Result<(Self, Self::Api)>` `#[setup]`. Exercises a client Publisher
// + a Latest + an OWNER-side Publisher built from `topic::owner()` (the path
// every real participant opens with),
// `#[step]`/`#[shutdown]` taking `&mut Self::Api`, an exclusive
// `#[server(api = …)]` taking `&mut Self::Api`, and a `#[server_snapshot(api
// = …)]` taking a read-only `&Self::Api` (D3) - the green proof that the
// codegen (`phoxal-macros::behavior`/`authoring`) and the `SetupContext`
// surface (`SetupContextApiExt`) work end to end.
//
// Imported as `v1` (not `as api`), matching the companion doc: the
// lifecycle parameters are conventionally named `api`, so aliasing the module
// to `api` too would shadow it inside method bodies.
use phoxal::api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {
    target_distance_m: f32,
}

#[derive(phoxal::Api)]
struct Api {
    target: CommandPublisher<api::drive::Target>,
    // OWNER-side publish of `drive/state`, built below from `topic::owner()`,
    // so this fixture exercises both explicit sides of the topic API.
    state: StatePublisher<api::drive::State>,
    odometry: Latest<api::drive::State>,
    lookup: Server<api::frame::LookupRequest, api::frame::LookupResponse>,
    submap: Server<api::map::SubmapRequest, api::map::SubmapResponse>,
}

struct WallFollowerSnapshot;

#[phoxal::service(id = "wall-follower-v2")]
struct WallFollower {
    last_error: f32,
}

#[phoxal::behavior]
impl WallFollower {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        config: Self::Config,
    ) -> Result<(Self, Self::Api)> {
        let _ = config.target_distance_m;
        // Owning `drive/state` uses the explicit owner builder.
        Ok((
            Self { last_error: 0.0 },
            Self::Api {
                target: ctx.command_publisher(api::topic::client().drive().target()).await?,
                state: ctx
                    .state_publisher(api::topic::owner().drive().state())
                    .await?,
                odometry: ctx.latest(api::topic::client().drive().state()).await?,
                lookup: ctx.server(api::topic::client().frame().lookup()).await?,
                submap: ctx.server(api::topic::client().map().submap()).await?,
            },
        ))
    }

    #[step(hz = 20)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let Some(odometry) = api.odometry.latest() else {
            return Ok(());
        };
        self.last_error = odometry.target.linear_x_mps;
        api.target.send(api::drive::Target {
                    linear_x_mps: 0.2,
                    angular_z_radps: 0.0,
                    curvature_limit_radpm: None,
                })?;
        // Owner-side publish of the drive state, over the owner topic bound
        // in `#[setup]`.
        api.state.publish(step.token(), api::drive::State {
                    target: odometry.target.clone(),
                    limited_target: odometry.target,
                    actuator_authority: api::drive::ActuatorAuthority::Active,
                    stop_reason: None,
                })?;
        Ok(())
    }

    #[server(api = lookup)]
    async fn lookup(
        &mut self,
        api: &mut Self::Api,
        request: api::frame::LookupRequest,
    ) -> ServerResult<api::frame::LookupResponse> {
        let _ = (&*api, &request);
        Ok(api::frame::LookupResponse { transform: None })
    }

    #[server_snapshot(api = submap)]
    async fn submap(
        state: Snapshot<WallFollowerSnapshot>,
        api: &Self::Api,
        request: api::map::SubmapRequest,
    ) -> ServerResult<api::map::SubmapResponse> {
        let _ = (state, api, request);
        Ok(api::map::SubmapResponse {
            width: 0,
            height: 0,
            resolution_m: 0.05,
            cells: Vec::new(),
        })
    }

    #[snapshot]
    fn snapshot(&self) -> WallFollowerSnapshot {
        WallFollowerSnapshot
    }

    #[shutdown]
    async fn shutdown(&mut self, api: &mut Self::Api, ctx: ShutdownContext) -> Result<()> {
        let _ = ctx;
        api.target.send(api::drive::Target {
                    linear_x_mps: 0.0,
                    angular_z_radps: 0.0,
                    curvature_limit_radpm: None,
                })?;
        Ok(())
    }
}

fn main() {}
