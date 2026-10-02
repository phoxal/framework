use crate::config::{WorldConfig, validate_config};
#[cfg(test)]
use crate::contract::world_api::Inputs as WorldInputs;
use crate::contract::{
    Bounds, GridWindow, Occupancy, UnavailableReason, WindowRequest, WindowResponse,
    WindowUnavailable, WindowUnavailableReason, WorldBelief, WorldRevision, WorldStatus,
};
use crate::validation;
#[cfg(test)]
use phoxal::contracts::robotics::OdometryState;
use phoxal::runtime::Context;
#[cfg(test)]
use phoxal::runtime::input::Latest;
use std::collections::VecDeque;

struct WorldSnapshot {
    window: GridWindow,
}
/// Private world state retained by the serialized compute owner.
pub struct WorldState {
    config: WorldConfig,
    belief: WorldBelief,
    revision: u64,
    available: bool,
    unavailable_reasons: Vec<UnavailableReason>,
    snapshots: VecDeque<WorldSnapshot>,
    applied_invocation: u64,
}

impl WorldState {
    fn new(config: WorldConfig) -> Self {
        Self {
            belief: WorldBelief {
                frame_id: config.frame_id.clone(),
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
                confidence: 0.0,
                revision: 0,
                available: false,
                oldest_capture_time_nanos: None,
            },
            config,
            revision: 0,
            available: false,
            unavailable_reasons: vec![UnavailableReason::Pose],
            snapshots: VecDeque::new(),
            applied_invocation: u64::MAX,
        }
    }

    fn revision_marker(&self) -> WorldRevision {
        WorldRevision {
            revision: self.revision,
            available: self.available,
            oldest_capture_time_nanos: self.belief.oldest_capture_time_nanos,
        }
    }

    fn status(&self) -> WorldStatus {
        WorldStatus {
            available: self.available,
            unavailable_reasons: self.unavailable_reasons.clone(),
            revision: self.revision,
        }
    }

    fn window(&self, requested: Bounds, revision: u64) -> GridWindow {
        let covered = self.covered_bounds();
        // Localization establishes a pose, not traversability. Mapping must
        // supply measured occupancy before a planner can treat a cell as free.
        let cells =
            vec![Occupancy::Unknown; self.config.width as usize * self.config.height as usize];
        GridWindow {
            frame_id: self.config.frame_id.clone(),
            origin_x_m: self.config.origin_x_m,
            origin_y_m: self.config.origin_y_m,
            resolution_m: self.config.resolution_m,
            width: self.config.width,
            height: self.config.height,
            cells,
            revision,
            requested: Some(requested),
            covered: Some(covered),
        }
    }

    fn covered_bounds(&self) -> Bounds {
        Bounds {
            min_x_m: self.config.origin_x_m,
            min_y_m: self.config.origin_y_m,
            max_x_m: self.config.origin_x_m
                + f64::from(self.config.width) * self.config.resolution_m,
            max_y_m: self.config.origin_y_m
                + f64::from(self.config.height) * self.config.resolution_m,
        }
    }

    fn retain_snapshot(&mut self, snapshot: WorldSnapshot) {
        if self.snapshots.len() == self.config.history_capacity as usize {
            self.snapshots.pop_front();
        }
        self.snapshots.push_back(snapshot);
    }
}

/// The official world service implementation.
pub struct World {
    state: WorldState,
}

#[phoxal::runtime(contract = crate::contract::WorldApi, period_ms = 20)]
impl World {
    #[init]
    fn new(config: WorldConfig) -> phoxal::Result<Self> {
        validate_config(&config)?;
        Ok(Self {
            state: WorldState::new(config),
        })
    }

    /// Serves one occupancy-window request from the current retained belief.
    ///
    /// The invocation's pose update is applied first when this handler is
    /// the first to observe the invocation, exactly as the unified step
    /// updated belief before serving window requests.
    #[handle(window)]
    fn serve_window(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: WindowRequest,
    ) -> phoxal::Result<WindowResponse> {
        self.apply_pose(ctx);
        let response = window_for(&self.state, &request);
        validation::window_response(&response).map_err(|error| anyhow::anyhow!(error))?;
        Ok(response)
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        self.apply_pose(ctx);
        validation::belief(&self.state.belief).map_err(|error| anyhow::anyhow!(error))?;
        validation::revision(&self.state.revision_marker())
            .map_err(|error| anyhow::anyhow!(error))?;
        validation::status(&self.state.status()).map_err(|error| anyhow::anyhow!(error))?;
        Ok(())
    }

    /// Projects the current estimated spatial belief.
    #[publish(belief)]
    fn belief(&self) -> WorldBelief {
        self.state.belief.clone()
    }

    /// Projects the coherent current revision marker.
    #[publish(revision)]
    fn revision(&self) -> WorldRevision {
        self.state.revision_marker()
    }

    /// Projects availability separately from the belief payload.
    #[publish(status)]
    fn status(&self) -> WorldStatus {
        self.state.status()
    }

    /// Applies this invocation's pose-derived update exactly once, so a
    /// window handler dispatched before the periodic step observes the
    /// same freshly updated state the unified step produced.
    fn apply_pose(&mut self, ctx: &Context<'_, Self>) {
        if self.state.applied_invocation == ctx.invocation_index() {
            return;
        }
        self.state.applied_invocation = ctx.invocation_index();
        let now = ctx.now();
        let max_age_ms = self.state.config.max_age_ms;
        let pose = ctx
            .pose()
            .fresh_within(max_age_ms)
            .filter(|pose| {
                validation::capture_is_fresh_at(
                    pose.oldest_capture_time_nanos,
                    now.as_nanos(),
                    max_age_ms.saturating_mul(1_000_000),
                )
            })
            .copied();
        if pose.is_none() {
            self.state.available = false;
            self.state.unavailable_reasons = vec![UnavailableReason::StalePose];
            self.state.belief.available = false;
        } else if pose.is_some_and(|pose| validation::odometry(&pose).is_err()) {
            self.state.available = false;
            self.state.unavailable_reasons = vec![UnavailableReason::InvalidPose];
            self.state.belief.available = false;
        } else if pose.is_some_and(|pose| !pose.available) {
            self.state.available = false;
            self.state.unavailable_reasons = vec![UnavailableReason::Pose];
            self.state.belief.available = false;
        } else if let Some(pose) = pose {
            self.state.revision = self.state.revision.saturating_add(1);
            self.state.belief = WorldBelief {
                frame_id: self.state.config.frame_id.clone(),
                x_m: pose.x_m,
                y_m: pose.y_m,
                yaw_rad: pose.yaw_rad,
                confidence: 1.0,
                revision: self.state.revision,
                available: true,
                oldest_capture_time_nanos: pose.oldest_capture_time_nanos,
            };
            self.state.available = true;
            self.state.unavailable_reasons.clear();
            let requested = self.state.covered_bounds();
            let window = self.state.window(requested, self.state.revision);
            self.state.retain_snapshot(WorldSnapshot { window });
        }
    }
}

fn window_for(state: &WorldState, request: &WindowRequest) -> WindowResponse {
    let Some(requested) = request.requested.as_ref() else {
        return unavailable(WindowUnavailableReason::WorldUnavailable, state.revision);
    };
    if validation::window_request(request).is_err() || !state.available {
        return unavailable(WindowUnavailableReason::WorldUnavailable, state.revision);
    }
    let window = if request.revision == 0 {
        state.snapshots.back().map(|snapshot| &snapshot.window)
    } else {
        state
            .snapshots
            .iter()
            .map(|snapshot| &snapshot.window)
            .find(|window| window.revision == request.revision)
    };
    let Some(window) = window else {
        return unavailable(WindowUnavailableReason::RevisionNotRetained, state.revision);
    };
    let Some(covered) = window.covered.as_ref() else {
        return unavailable(WindowUnavailableReason::WorldUnavailable, state.revision);
    };
    if requested.min_x_m < covered.min_x_m
        || requested.min_y_m < covered.min_y_m
        || requested.max_x_m > covered.max_x_m
        || requested.max_y_m > covered.max_y_m
    {
        return unavailable(WindowUnavailableReason::OutOfBounds, window.revision);
    }
    let mut selected = window.clone();
    selected.requested = Some(*requested);
    WindowResponse::Window(selected)
}

fn unavailable(reason: WindowUnavailableReason, revision: u64) -> WindowResponse {
    WindowResponse::Unavailable(WindowUnavailable { reason, revision })
}

#[cfg(test)]
mod tests {
    use phoxal::runtime::{
        ExecutionDuration, ExecutionTime, ObservationStamp, RuntimeOwner, Sample, StepContext,
        invoke,
    };

    use super::phoxal_runtime_world;
    use super::*;

    fn context(index: u64, now_ms: u64, previous_ms: Option<u64>) -> StepContext {
        StepContext::from_previous(
            ExecutionTime::from_nanos(now_ms * 1_000_000),
            ExecutionDuration::from_millis(20),
            previous_ms.map(|at| ExecutionTime::from_nanos(at * 1_000_000)),
            0,
            index,
        )
    }

    fn step(world: World, context: &StepContext, inputs: &WorldInputs) -> World {
        let (world, _) = invoke(
            &phoxal_runtime_world::Adapter::new(),
            context,
            world,
            inputs,
        )
        .expect("the world step accepts the inputs");
        world
    }

    fn pose(at_ms: u64, source_revision: u64) -> Latest<OdometryState> {
        Latest::from_sample(Sample::new(
            OdometryState {
                x_m: 0.2,
                y_m: -0.1,
                yaw_rad: 0.0,
                linear_x_mps: 0.0,
                angular_z_radps: 0.0,
                revision: source_revision,
                available: true,
                oldest_capture_time_nanos: Some(at_ms * 1_000_000),
            },
            ObservationStamp::new(
                "kinematics",
                ExecutionTime::from_nanos(at_ms * 1_000_000),
                Some(source_revision),
            ),
        ))
    }

    #[test]
    fn fresh_pose_creates_coherent_revisioned_belief_and_window() {
        let world = step(
            World::new(WorldConfig::default()).expect("valid config"),
            &context(0, 20, None),
            &WorldInputs {
                pose: pose(20, 7),
                window: Default::default(),
            },
        );
        let state = &world.state;
        assert!(state.available);
        assert_eq!(state.revision, 1);
        let request = WindowRequest {
            requested: Some(Bounds {
                min_x_m: 0.2,
                min_y_m: 0.2,
                max_x_m: 0.8,
                max_y_m: 0.8,
            }),
            revision: 1,
        };
        let response = window_for(state, &request);
        validation::window_response(&response).expect("retained window response");
        let WindowResponse::Window(window) = response else {
            panic!("available window")
        };
        assert!(
            window.cells.iter().all(|cell| *cell == Occupancy::Unknown),
            "a pose observation does not establish free space"
        );
    }

    #[test]
    fn republishing_pose_preserves_capture_age_and_rejects_stale_or_future_evidence() {
        for capture in [Some(0), Some(200_000_001), None] {
            let mut value = *pose(200, 7).value().unwrap();
            value.oldest_capture_time_nanos = capture;
            let world = step(
                World::new(WorldConfig::default()).expect("valid config"),
                &context(0, 200, None),
                &WorldInputs {
                    pose: Latest::from_sample(Sample::new(
                        value,
                        ObservationStamp::new(
                            "kinematics",
                            ExecutionTime::from_nanos(200_000_000),
                            None,
                        ),
                    )),
                    window: Default::default(),
                },
            );
            assert!(
                !world.state.available,
                "fresh publication cannot renew {capture:?}"
            );
        }
        let mut value = *pose(100, 7).value().unwrap();
        value.oldest_capture_time_nanos = Some(50_000_000);
        let world = step(
            World::new(WorldConfig::default()).expect("valid config"),
            &context(0, 100, None),
            &WorldInputs {
                pose: Latest::from_sample(Sample::new(
                    value,
                    ObservationStamp::new(
                        "kinematics",
                        ExecutionTime::from_nanos(100_000_000),
                        None,
                    ),
                )),
                window: Default::default(),
            },
        );
        assert!(world.state.available);
        assert_eq!(
            world.state.belief.oldest_capture_time_nanos,
            Some(50_000_000)
        );
        assert_eq!(
            world.state.revision_marker().oldest_capture_time_nanos,
            Some(50_000_000)
        );
    }

    #[test]
    fn stale_pose_is_unavailable_instead_of_reusing_old_belief() {
        let world = step(
            World::new(WorldConfig::default()).expect("valid config"),
            &context(0, 200, None),
            &WorldInputs {
                pose: pose(20, 7),
                window: Default::default(),
            },
        );
        assert!(!world.state.available);
        assert_eq!(world.state.revision, 0);
        assert_eq!(
            world.state.unavailable_reasons,
            vec![UnavailableReason::StalePose]
        );
    }

    #[test]
    fn invalid_config_is_rejected_before_initialization() {
        let config = WorldConfig {
            width: 0,
            ..WorldConfig::default()
        };
        assert!(
            RuntimeOwner::new(
                phoxal_runtime_world::Adapter::new(),
                ExecutionTime::from_nanos(0),
                config
            )
            .is_err()
        );
    }
}
