//! `explore` - frontier-based goal proposal.
//!
//! A scheduled participant that subscribes to `map/revision` and `localize/state`,
//! queries the `map/submap` server, and publishes ranked `explore/frontiers`
//! plus an `explore/state` (whether it is exploring and the selected frontier).
//! Each step it detects free/unknown boundary cells in the returned grid, scores
//! them by size over distance to the robot, and selects the top-ranked frontier.
//! The current map query response contains a grid but not an explicit grid
//! origin. This participant therefore requests a fixed map-frame window anchored at
//! `(0, 0)` and interprets the response in that same frame.
//! It does nothing until both a map revision and a trusted (positive-confidence,
//! finite) localization estimate are available.
//! If the map server is unavailable or returns an invalid grid, no frontiers are
//! fabricated and it reports that it is not exploring.

mod frontiers;
mod scoring;

use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::v1 as api;

use crate::frontiers::{OccupancyGrid, detect_frontiers};
use crate::scoring::score_frontiers;

const SUBMAP_WINDOW_CELLS: f64 = 128.0;

#[derive(phoxal::Api)]
struct Api {
    map_revision: Latest<api::map::Revision>,
    localize: Latest<api::localize::LocalizationState>,
    submap: Querier<api::map::SubmapRequest, api::map::SubmapResponse>,
    frontiers: Publisher<api::explore::Frontiers>,
    state: Publisher<api::explore::State>,
}

#[phoxal::service(id = "explore", config = ())]
struct Explore {}

#[phoxal::behavior]
impl Explore {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        Ok((
            Self {},
            Self::Api {
                map_revision: ctx.latest(api::topic::new().map().revision()).await?,
                localize: ctx.latest(api::topic::new().localize().state()).await?,
                submap: ctx.querier(api::topic::new().map().submap()).await?,
                // Explore OWNS the `explore` node (its frontiers + state telemetry) ->
                // owner (`internal`) builder; `map/submap` is CLIENT-queried and the
                // subscriptions are CONSUMED via the public builder.
                frontiers: ctx
                    .publisher(api::topic::internal::new(cap).explore().frontiers())
                    .await?,
                state: ctx
                    .publisher(api::topic::internal::new(cap).explore().state())
                    .await?,
            },
        ))
    }

    #[step(hz = 2)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let Some(map_revision) = api.map_revision.latest() else {
            Self::publish_state(&api.state, step.time(), false, None).await?;
            return Ok(());
        };
        let Some(localize) = api.localize.latest() else {
            Self::publish_state(&api.state, step.time(), false, None).await?;
            return Ok(());
        };
        if !valid_localization(&localize) {
            Self::publish_state(&api.state, step.time(), false, None).await?;
            return Ok(());
        }

        let Some(request) = submap_request(&map_revision) else {
            Self::publish_state(&api.state, step.time(), false, None).await?;
            return Ok(());
        };
        let response = match api.submap.query(request.clone()).await {
            Ok(response) => response,
            Err(_) => {
                Self::publish_state(&api.state, step.time(), false, None).await?;
                return Ok(());
            }
        };

        let frontiers = evaluate_frontiers(&request, response, (localize.x_m, localize.y_m))
            .unwrap_or_default();
        let selected = frontiers.first().cloned();
        api.frontiers
            .publish_at(
                step.time(),
                api::explore::Frontiers {
                    frontiers,
                    map_revision: Some(map_revision.revision),
                },
            )
            .await?;
        Self::publish_state(&api.state, step.time(), selected.is_some(), selected).await?;
        Ok(())
    }
}

impl Explore {
    async fn publish_state(
        state: &Publisher<api::explore::State>,
        at: LogicalTime,
        exploring: bool,
        selected: Option<api::explore::Frontier>,
    ) -> Result<()> {
        state
            .publish_at(
                at,
                api::explore::State {
                    exploring,
                    selected,
                },
            )
            .await?;
        Ok(())
    }
}

fn submap_request(map_revision: &api::map::Revision) -> Option<api::map::SubmapRequest> {
    if !map_revision.resolution_m.is_finite() || map_revision.resolution_m <= 0.0 {
        return None;
    }
    let extent_m = (f64::from(map_revision.resolution_m) * SUBMAP_WINDOW_CELLS)
        .max(f64::from(map_revision.resolution_m));
    Some(api::map::SubmapRequest {
        min_x_m: 0.0,
        min_y_m: 0.0,
        max_x_m: extent_m,
        max_y_m: extent_m,
    })
}

fn evaluate_frontiers(
    request: &api::map::SubmapRequest,
    response: api::map::SubmapResponse,
    robot_xy_m: (f64, f64),
) -> Option<Vec<api::explore::Frontier>> {
    let grid = OccupancyGrid::from_submap(request, response)?;
    let frontiers = detect_frontiers(&grid);
    Some(score_frontiers(frontiers, &grid, robot_xy_m))
}

fn valid_localization(localize: &api::localize::LocalizationState) -> bool {
    localize.x_m.is_finite()
        && localize.y_m.is_finite()
        && localize.yaw_rad.is_finite()
        && localize.confidence.is_finite()
        && localize.confidence > 0.0
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Explore>()
}

#[cfg(test)]
mod tests {
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};
    use phoxal_api::ContractBody;
    use phoxal_api::v1 as api;

    use super::{Explore, evaluate_frontiers, submap_request};

    #[test]
    fn request_uses_fixed_origin_and_revision_resolution() {
        let request = submap_request(&api::map::Revision {
            revision: 3,
            resolution_m: 0.05,
        })
        .unwrap();

        assert_eq!(request.min_x_m, 0.0);
        assert_eq!(request.min_y_m, 0.0);
        assert!((request.max_x_m - 6.4).abs() < 1e-6);
        assert!((request.max_y_m - 6.4).abs() < 1e-6);
    }

    #[test]
    fn request_rejects_invalid_resolution() {
        assert!(
            submap_request(&api::map::Revision {
                revision: 3,
                resolution_m: 0.0,
            })
            .is_none()
        );
    }

    #[test]
    fn evaluate_frontiers_detects_and_scores_grid_frontiers() {
        let request = api::map::SubmapRequest {
            min_x_m: 0.0,
            min_y_m: 0.0,
            max_x_m: 3.0,
            max_y_m: 2.0,
        };
        let response = api::map::SubmapResponse {
            width: 3,
            height: 2,
            resolution_m: 1.0,
            cells: vec![0, 0, 255, 0, 0, 255],
        };

        let frontiers = evaluate_frontiers(&request, response, (0.5, 0.5)).unwrap();

        assert_eq!(frontiers.len(), 1);
        assert_eq!(frontiers[0].size, 2);
        assert_eq!(frontiers[0].x_m, 1.5);
        assert_eq!(frontiers[0].y_m, 1.0);
        assert!(frontiers[0].score > 0.0);
    }

    #[test]
    fn invalid_response_yields_no_frontier_result() {
        let request = api::map::SubmapRequest {
            min_x_m: 0.0,
            min_y_m: 0.0,
            max_x_m: 2.0,
            max_y_m: 2.0,
        };
        let response = api::map::SubmapResponse {
            width: 2,
            height: 2,
            resolution_m: 1.0,
            cells: vec![0; 3],
        };

        assert!(evaluate_frontiers(&request, response, (0.0, 0.0)).is_none());
    }

    #[test]
    fn api_reports_explore_contracts() {
        assert_eq!(<Explore as Participant>::ID, "explore");

        let contracts = <<Explore as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert_contract::<api::map::Revision>(contracts, ContractRole::Subscribe);
        assert_contract::<api::localize::LocalizationState>(contracts, ContractRole::Subscribe);
        assert_contract::<api::map::SubmapRequest>(contracts, ContractRole::Ask);
        assert_contract::<api::map::SubmapResponse>(contracts, ContractRole::Ask);
        assert_contract::<api::explore::Frontiers>(contracts, ContractRole::Publish);
        assert_contract::<api::explore::State>(contracts, ContractRole::Publish);
    }

    fn assert_contract<B>(contracts: &[phoxal::participant::ApiContractUse], role: ContractRole)
    where
        B: ContractBody,
    {
        assert!(
            contracts
                .iter()
                .any(|c| c.topic == B::TOPIC && c.role == role)
        );
    }
}
