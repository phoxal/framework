//! `explore` - frontier-based goal proposal.
//!
//! The current map query response contains a grid but not an explicit grid
//! origin. This runtime therefore requests a fixed map-frame window anchored at
//! `(0, 0)` and interprets the response in that same frame. If the map server is
//! unavailable or returns an invalid grid, no frontiers are fabricated.

mod frontiers;
mod scoring;

use anyhow::Result;
use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

use crate::frontiers::{OccupancyGrid, detect_frontiers};
use crate::scoring::score_frontiers;

const SUBMAP_WINDOW_CELLS: f64 = 128.0;

#[derive(phoxal::Runtime)]
#[phoxal(id = "explore", api = y2026_1)]
struct Explore {
    map_revision: Latest<api::map::Revision>,
    localize: Latest<api::localize::LocalizationState>,
    submap: Querier<api::map::SubmapRequest, api::map::SubmapResponse>,
    frontiers: Publisher<api::explore::Frontiers>,
    state: Publisher<api::explore::State>,
}

#[phoxal::runtime]
impl Explore {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            map_revision: ctx
                .subscribe(api::topic::new().map().revision())
                .latest()
                .await?,
            localize: ctx
                .subscribe(api::topic::new().localize().state())
                .latest()
                .await?,
            submap: ctx.querier(api::topic::new().map().submap()).await?,
            frontiers: ctx
                .publisher(api::topic::new().explore().frontiers())
                .await?,
            state: ctx.publisher(api::topic::new().explore().state()).await?,
        })
    }

    #[step(hz = 2)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let Some(map_revision) = self.map_revision.latest() else {
            self.publish_state(step.time(), false, None).await?;
            return Ok(());
        };
        let Some(localize) = self.localize.latest() else {
            self.publish_state(step.time(), false, None).await?;
            return Ok(());
        };
        if !valid_localization(&localize) {
            self.publish_state(step.time(), false, None).await?;
            return Ok(());
        }

        let Some(request) = submap_request(&map_revision) else {
            self.publish_state(step.time(), false, None).await?;
            return Ok(());
        };
        let response = match self.submap.query(request.clone()).await {
            Ok(response) => response,
            Err(_) => {
                self.publish_state(step.time(), false, None).await?;
                return Ok(());
            }
        };

        let frontiers = evaluate_frontiers(&request, response, (localize.x_m, localize.y_m))
            .unwrap_or_default();
        let selected = frontiers.first().cloned();
        self.frontiers
            .publish_at(
                step.time(),
                api::explore::Frontiers {
                    frontiers,
                    map_revision: Some(map_revision.revision),
                },
            )
            .await?;
        self.publish_state(step.time(), selected.is_some(), selected)
            .await?;
        Ok(())
    }
}

impl Explore {
    async fn publish_state(
        &self,
        at: LogicalTime,
        exploring: bool,
        selected: Option<api::explore::Frontier>,
    ) -> Result<()> {
        self.state
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
    use phoxal::api::ContractBody;
    use phoxal::api::y2026_1 as api;

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
    fn emit_apis_reports_explore_contracts() {
        let json = phoxal::runtime::emit_apis_json::<Explore>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "explore");
        assert_eq!(value["api_version"], "y2026_1");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert_contract::<api::map::Revision>(contracts, "subscribe");
        assert_contract::<api::localize::LocalizationState>(contracts, "subscribe");
        assert_contract::<api::map::SubmapRequest>(contracts, "query_request");
        assert_contract::<api::map::SubmapResponse>(contracts, "query_response");
        assert_contract::<api::explore::Frontiers>(contracts, "publish");
        assert_contract::<api::explore::State>(contracts, "publish");
    }

    fn assert_contract<B>(contracts: &[serde_json::Value], direction: &str)
    where
        B: ContractBody,
    {
        assert!(contracts.iter().any(|c| {
            c["family"] == B::FAMILY && c["topic"] == B::TOPIC && c["direction"] == direction
        }));
    }
}
