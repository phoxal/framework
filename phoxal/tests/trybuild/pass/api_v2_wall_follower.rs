// NEW authoring model fixture (phoxal-api-refactor "remove emit-apis"
// proposal): `#[derive(phoxal::Api)]` + `#[derive(phoxal::Config)]` +
// `#[phoxal::service]` + `#[phoxal::behavior]` with a `Result<(Self,
// Self::Api)>` `#[setup]`. Exercises a client Publisher + a Latest + an
// OWNER-side Publisher built from `ctx.owner_capability()` /
// `topic::internal::new(cap)` (the path every real participant opens with),
// `#[step]`/`#[shutdown]` taking `&mut Self::Api`, an exclusive
// `#[server(api = …)]` taking `&mut Self::Api`, and a `#[server_snapshot(api
// = …)]` taking a read-only `&Self::Api` (D3) - the green proof that the new
// codegen (`phoxal-macros::behavior_v2`/`authoring_v2`) and the new-model
// `SetupContext` surface (`SetupContextApiExt`, incl. `owner_capability`)
// work end to end, coexisting with the OLD-model fixtures in this same
// directory (e.g. `control_loop.rs`, `server_snapshot.rs`), which stay on
// `#[derive(phoxal::Service)]` / bare `Result<Self>` untouched.
//
// Imported as `y2026_1` (not `as api`), matching the companion doc: the new
// model's lifecycle parameters are conventionally named `api`, so aliasing
// the module to `api` too would shadow it inside method bodies.
use phoxal_api::y2026_1;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {
    target_distance_m: f32,
}

#[derive(phoxal::Api)]
struct Api {
    target: Publisher<y2026_1::drive::Target>,
    // OWNER-side publish of `drive/state`, built below from an owner-capability
    // internal topic (`topic::internal::new(cap)`), so this fixture exercises
    // the owner-cap path P-convert (battery, presence, …) starts every real
    // participant with - not just the public client topics.
    state: Publisher<y2026_1::drive::State>,
    odometry: Latest<y2026_1::drive::State>,
    lookup: Server<y2026_1::frame::LookupRequest, y2026_1::frame::LookupResponse>,
    submap: Server<y2026_1::map::SubmapRequest, y2026_1::map::SubmapResponse>,
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
        // Every real participant starts here; owning `drive/state` requires the
        // runner-minted capability passed to the `internal` owner builder.
        let cap = ctx.owner_capability();
        Ok((
            Self { last_error: 0.0 },
            Self::Api {
                target: ctx.publisher(y2026_1::topic::new().drive().target()).await?,
                state: ctx
                    .publisher(y2026_1::topic::internal::new(cap).drive().state())
                    .await?,
                odometry: ctx.latest(y2026_1::topic::new().drive().state()).await?,
                lookup: ctx.server(y2026_1::topic::new().frame().lookup()).await?,
                submap: ctx.server(y2026_1::topic::new().map().submap()).await?,
            },
        ))
    }

    #[step(hz = 20)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let Some(odometry) = api.odometry.latest() else {
            return Ok(());
        };
        self.last_error = odometry.target.linear_x_mps;
        api.target
            .publish_at(
                step.time(),
                y2026_1::drive::Target {
                    linear_x_mps: 0.2,
                    angular_z_radps: 0.0,
                    curvature_limit_radpm: None,
                },
            )
            .await?;
        // Owner-side publish of the drive state, over the internal topic bound
        // in `#[setup]`.
        api.state
            .publish_at(
                step.time(),
                y2026_1::drive::State {
                    target: odometry.target.clone(),
                    limited_target: odometry.target,
                    actuator_authority: y2026_1::drive::ActuatorAuthority::Active,
                    stop_reason: None,
                },
            )
            .await?;
        Ok(())
    }

    #[server(api = lookup)]
    async fn lookup(
        &mut self,
        api: &mut Self::Api,
        request: y2026_1::frame::LookupRequest,
    ) -> ServerResult<y2026_1::frame::LookupResponse> {
        let _ = (&*api, &request);
        Ok(y2026_1::frame::LookupResponse { transform: None })
    }

    #[server_snapshot(api = submap)]
    async fn submap(
        state: Snapshot<WallFollowerSnapshot>,
        api: &Self::Api,
        request: y2026_1::map::SubmapRequest,
    ) -> ServerResult<y2026_1::map::SubmapResponse> {
        let _ = (state, api, request);
        Ok(y2026_1::map::SubmapResponse {
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
        api.target
            .publish_at(
                LogicalTime::new(0, 0),
                y2026_1::drive::Target {
                    linear_x_mps: 0.0,
                    angular_z_radps: 0.0,
                    curvature_limit_radpm: None,
                },
            )
            .await?;
        Ok(())
    }
}

fn main() {}
