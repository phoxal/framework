#[cfg(test)]
use self::phoxal_provider::Inputs as WorldInputs;
#[cfg(test)]
use crate::api::types::phoxal::kinematics::v1::OdometryState;
use crate::api::types::phoxal::world::v1::{
    Bounds, GridWindow, Occupancy, UnavailableReason, WindowRequest, WindowResponse,
    WindowUnavailable, WindowUnavailableReason, WorldBelief, WorldRevision, WorldStatus,
    window_response,
};
use crate::config::{WorldConfig, validate_config};
use crate::validation;
#[cfg(test)]
use phoxal::runtime::input::Latest;
use phoxal::runtime::{InitContext, Runtime, StepContext};
use std::collections::VecDeque;

struct WorldSnapshot {
    window: GridWindow,
}
/// Private world state retained by the serialized Runtime owner.
pub struct WorldState {
    config: WorldConfig,
    belief: WorldBelief,
    revision: u64,
    available: bool,
    unavailable_reasons: Vec<i32>,
    snapshots: VecDeque<WorldSnapshot>,
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
            unavailable_reasons: vec![UnavailableReason::Pose as i32],
            snapshots: VecDeque::new(),
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
        let cells = vec![
            Occupancy::Unknown as i32;
            self.config.width as usize * self.config.height as usize
        ];
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
#[derive(Clone, Copy, Debug, Default)]
pub struct World;

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for World {
    type Config = WorldConfig;
    type State = WorldState;

    fn validate_config(config: &Self::Config) -> phoxal::Result<()> {
        validate_config(config)
    }

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(WorldState::new(config))
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        inputs
            .window
            .validate_order()
            .map_err(|error| anyhow::anyhow!(error))?;
        let pose = inputs
            .pose
            .is_fresh_at(ctx.now(), Some(state.config.max_age_ms))
            .then(|| inputs.pose.value())
            .flatten()
            .filter(|pose| {
                validation::capture_is_fresh_at(
                    pose.oldest_capture_time_nanos,
                    ctx.now().as_nanos(),
                    state.config.max_age_ms.saturating_mul(1_000_000),
                )
            });
        if pose.is_none() {
            state.available = false;
            state.unavailable_reasons = vec![UnavailableReason::StalePose as i32];
            state.belief.available = false;
        } else if pose.is_some_and(|pose| validation::odometry(pose).is_err()) {
            state.available = false;
            state.unavailable_reasons = vec![UnavailableReason::InvalidPose as i32];
            state.belief.available = false;
        } else if pose.is_some_and(|pose| !pose.available) {
            state.available = false;
            state.unavailable_reasons = vec![UnavailableReason::Pose as i32];
            state.belief.available = false;
        } else if let Some(pose) = pose {
            state.revision = state.revision.saturating_add(1);
            state.belief = WorldBelief {
                frame_id: state.config.frame_id.clone(),
                x_m: pose.x_m,
                y_m: pose.y_m,
                yaw_rad: pose.yaw_rad,
                confidence: 1.0,
                revision: state.revision,
                available: true,
                oldest_capture_time_nanos: pose.oldest_capture_time_nanos,
            };
            state.available = true;
            state.unavailable_reasons.clear();
            let requested = state.covered_bounds();
            let window = state.window(requested, state.revision);
            state.retain_snapshot(WorldSnapshot { window });
        }

        validation::belief(&state.belief).map_err(|error| anyhow::anyhow!(error))?;
        validation::revision(&state.revision_marker()).map_err(|error| anyhow::anyhow!(error))?;
        validation::status(&state.status()).map_err(|error| anyhow::anyhow!(error))?;
        let mut outputs = Self::Outputs::default();
        for command in inputs.window.items() {
            let response = window_for(&state, command.request());
            validation::window_response(&response).map_err(|error| anyhow::anyhow!(error))?;
            outputs.window_replies.push(command.reply(response));
        }
        Ok((state, outputs))
    }
}

impl crate::api::projections::Projections for World {
    type State = WorldState;

    /// Projects the current estimated spatial belief.
    fn belief(&self, state: &WorldState) -> WorldBelief {
        state.belief.clone()
    }

    /// Projects the coherent current revision marker.
    fn revision(&self, state: &WorldState) -> WorldRevision {
        state.revision_marker()
    }

    /// Projects availability separately from the belief payload.
    fn status(&self, state: &WorldState) -> WorldStatus {
        state.status()
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
    WindowResponse {
        result: Some(window_response::Result::Window(selected)),
    }
}

fn unavailable(reason: WindowUnavailableReason, revision: u64) -> WindowResponse {
    WindowResponse {
        result: Some(window_response::Result::Unavailable(WindowUnavailable {
            reason: reason as i32,
            revision,
        })),
    }
}

#[cfg(test)]
mod tests {
    use phoxal::runtime::{
        ExecutionDuration, ExecutionTime, ObservationStamp, RuntimeOwner, Sample, StepContext,
    };

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
        let state = WorldState::new(WorldConfig::default());
        let (state, _) = World
            .step(
                &context(0, 20, None),
                state,
                &WorldInputs {
                    pose: pose(20, 7),
                    window: Default::default(),
                },
            )
            .expect("fresh pose");
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
        let response = window_for(&state, &request);
        validation::window_response(&response).expect("retained window response");
        let Some(window_response::Result::Window(window)) = response.result else {
            panic!("available window")
        };
        assert!(
            window
                .cells
                .iter()
                .all(|cell| *cell == Occupancy::Unknown as i32),
            "a pose observation does not establish free space"
        );
    }

    #[test]
    fn republishing_pose_preserves_capture_age_and_rejects_stale_or_future_evidence() {
        for capture in [Some(0), Some(200_000_001), None] {
            let mut value = *pose(200, 7).value().unwrap();
            value.oldest_capture_time_nanos = capture;
            let (state, _) = World
                .step(
                    &context(0, 200, None),
                    WorldState::new(WorldConfig::default()),
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
                )
                .unwrap();
            assert!(
                !state.available,
                "fresh publication cannot renew {capture:?}"
            );
        }
        let mut value = *pose(100, 7).value().unwrap();
        value.oldest_capture_time_nanos = Some(50_000_000);
        let (state, _) = World
            .step(
                &context(0, 100, None),
                WorldState::new(WorldConfig::default()),
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
            )
            .unwrap();
        assert!(state.available);
        assert_eq!(state.belief.oldest_capture_time_nanos, Some(50_000_000));
        assert_eq!(
            state.revision_marker().oldest_capture_time_nanos,
            Some(50_000_000)
        );
    }

    #[test]
    fn stale_pose_is_unavailable_instead_of_reusing_old_belief() {
        let state = WorldState::new(WorldConfig::default());
        let (state, _) = World
            .step(
                &context(0, 200, None),
                state,
                &WorldInputs {
                    pose: pose(20, 7),
                    window: Default::default(),
                },
            )
            .expect("stale pose is a valid transition");
        assert!(!state.available);
        assert_eq!(state.revision, 0);
        assert_eq!(
            state.unavailable_reasons,
            vec![UnavailableReason::StalePose as i32]
        );
    }

    #[test]
    fn invalid_config_is_rejected_before_initialization() {
        let config = WorldConfig {
            width: 0,
            ..WorldConfig::default()
        };
        assert!(RuntimeOwner::new(World, ExecutionTime::from_nanos(0), config).is_err());
    }
}
