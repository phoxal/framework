//! The official world Runtime.
//!
//! World owns a bounded spatial-belief transition and immutable revisioned
//! windows.  The first implementation is intentionally a deterministic free
//! space fixture: it copies a fresh pose observation into a world belief and
//! serves a configured finite window.  It does not claim SLAM, persistence,
//! perception, or a mature localization algorithm.

use std::collections::VecDeque;

use phoxal::runtime::input::Latest;
use phoxal::runtime::{InitContext, Runtime, StepContext};
use phoxal_kinematics::OdometryState;
use phoxal_world::{
    Bounds, GridWindow, Occupancy, UnavailableReason, WindowRequest, WindowResponse,
    WindowUnavailable, WindowUnavailableReason, WorldBelief, WorldRevision, WorldStatus, ports,
    window_response,
};

const DEFAULT_MAX_AGE_MS: u64 = 100;
const DEFAULT_HISTORY_CAPACITY: u32 = 64;
const DEFAULT_FRAME_ID: &str = "odom";
const DEFAULT_WIDTH: u32 = 64;
const DEFAULT_HEIGHT: u32 = 64;
const DEFAULT_RESOLUTION_M: f64 = 0.1;

/// Typed, validated configuration for one bounded world instance.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct WorldConfig {
    /// Frame in which the admitted pose and generated window are expressed.
    #[serde(default = "default_frame_id")]
    pub frame_id: String,
    /// Lower-left x coordinate of the controlled map fixture.
    #[serde(default)]
    pub origin_x_m: f64,
    /// Lower-left y coordinate of the controlled map fixture.
    #[serde(default)]
    pub origin_y_m: f64,
    /// Number of cells along x.
    #[serde(default = "default_width")]
    pub width: u32,
    /// Number of cells along y.
    #[serde(default = "default_height")]
    pub height: u32,
    /// Cell resolution in metres.
    #[serde(default = "default_resolution_m")]
    pub resolution_m: f64,
    /// Maximum admitted pose age in logical milliseconds.
    #[serde(default = "default_max_age_ms")]
    pub max_age_ms: u64,
    /// Number of world snapshots retained for immutable reads.
    #[serde(default = "default_history_capacity")]
    pub history_capacity: u32,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            frame_id: default_frame_id(),
            origin_x_m: 0.0,
            origin_y_m: 0.0,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            resolution_m: DEFAULT_RESOLUTION_M,
            max_age_ms: DEFAULT_MAX_AGE_MS,
            history_capacity: DEFAULT_HISTORY_CAPACITY,
        }
    }
}

fn default_frame_id() -> String {
    DEFAULT_FRAME_ID.to_owned()
}

const fn default_width() -> u32 {
    DEFAULT_WIDTH
}

const fn default_height() -> u32 {
    DEFAULT_HEIGHT
}

const fn default_resolution_m() -> f64 {
    DEFAULT_RESOLUTION_M
}

const fn default_max_age_ms() -> u64 {
    DEFAULT_MAX_AGE_MS
}

const fn default_history_capacity() -> u32 {
    DEFAULT_HISTORY_CAPACITY
}

fn validate_config(config: &WorldConfig) -> phoxal::Result<()> {
    if config.frame_id.is_empty() || config.frame_id.len() > phoxal_world::MAX_ID_BYTES {
        return Err(anyhow::anyhow!(
            "frame_id must contain 1 to {} UTF-8 bytes",
            phoxal_world::MAX_ID_BYTES
        ));
    }
    if !config.origin_x_m.is_finite() || !config.origin_y_m.is_finite() {
        return Err(anyhow::anyhow!("world origin must be finite"));
    }
    if config.width == 0 || config.height == 0 {
        return Err(anyhow::anyhow!("world width and height must be positive"));
    }
    if !config.resolution_m.is_finite() || config.resolution_m <= 0.0 {
        return Err(anyhow::anyhow!("resolution_m must be finite and positive"));
    }
    if config.max_age_ms == 0 || config.history_capacity == 0 {
        return Err(anyhow::anyhow!(
            "max_age_ms and history_capacity must be positive"
        ));
    }
    let cells = usize::try_from(config.width)
        .ok()
        .and_then(|width| {
            usize::try_from(config.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| anyhow::anyhow!("world dimensions overflow"))?;
    if cells > 1_048_576 {
        return Err(anyhow::anyhow!(
            "world fixture is limited to 1,048,576 cells"
        ));
    }
    let max_x_m = config.origin_x_m + f64::from(config.width) * config.resolution_m;
    let max_y_m = config.origin_y_m + f64::from(config.height) * config.resolution_m;
    if !max_x_m.is_finite() || !max_y_m.is_finite() {
        return Err(anyhow::anyhow!("world bounds must be finite"));
    }
    Ok(())
}

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
        let cells =
            vec![Occupancy::Free as i32; self.config.width as usize * self.config.height as usize];
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

/// One immutable pose cut for World.
#[phoxal::runtime::inputs]
pub struct WorldInputs {
    /// The latest kinematics-owned odometry, retaining its capture stamp.
    #[phoxal::runtime::input(max_age_ms = 100)]
    pub pose: Latest<OdometryState>,
}

/// World has no transient products.  Belief, revision, status, and immutable
/// windows are projections of the serialized state.
pub type WorldOutputs = ();

/// The official world service implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct World;

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for World {
    type Config = WorldConfig;
    type State = WorldState;
    type Inputs = WorldInputs;
    type Outputs = WorldOutputs;

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
        let Some(pose) = inputs
            .pose
            .is_fresh_at(ctx.now(), Some(state.config.max_age_ms))
            .then(|| inputs.pose.value())
            .flatten()
        else {
            state.available = false;
            state.unavailable_reasons = vec![UnavailableReason::StalePose as i32];
            state.belief.available = false;
            return Ok((state, ()));
        };
        if pose.validate().is_err() {
            state.available = false;
            state.unavailable_reasons = vec![UnavailableReason::InvalidPose as i32];
            state.belief.available = false;
            return Ok((state, ()));
        }
        if !pose.available {
            state.available = false;
            state.unavailable_reasons = vec![UnavailableReason::Pose as i32];
            state.belief.available = false;
            return Ok((state, ()));
        }

        state.revision = state.revision.saturating_add(1);
        state.belief = WorldBelief {
            frame_id: state.config.frame_id.clone(),
            x_m: pose.x_m,
            y_m: pose.y_m,
            yaw_rad: pose.yaw_rad,
            confidence: 1.0,
            revision: state.revision,
            available: true,
        };
        state.available = true;
        state.unavailable_reasons.clear();
        let requested = state.covered_bounds();
        let window = state.window(requested, state.revision);
        state.retain_snapshot(WorldSnapshot { window });
        Ok((state, ()))
    }
}

#[phoxal::runtime::outputs]
#[allow(
    dead_code,
    reason = "the collected projections are invoked by the transport runner"
)]
impl World {
    /// Projects the current estimated spatial belief.
    #[phoxal::runtime::outputs::state(port = ports::BELIEF, max_bytes = 512, bootstrap, on_change)]
    fn belief(&self, state: &WorldState) -> WorldBelief {
        state.belief.clone()
    }

    /// Projects the coherent current revision marker.
    #[phoxal::runtime::outputs::state(port = ports::REVISION, max_bytes = 128, bootstrap, on_change)]
    fn revision(&self, state: &WorldState) -> WorldRevision {
        state.revision_marker()
    }

    /// Projects availability separately from the belief payload.
    #[phoxal::runtime::outputs::state(port = ports::STATUS, max_bytes = 512, bootstrap, on_change)]
    fn status(&self, state: &WorldState) -> WorldStatus {
        state.status()
    }

    fn read_view(&self, state: &WorldState) -> WorldReadView {
        WorldReadView {
            available: state.available,
            current_revision: state.revision,
            current: state
                .snapshots
                .back()
                .map(|snapshot| snapshot.window.clone()),
            history: state
                .snapshots
                .iter()
                .map(|snapshot| snapshot.window.clone())
                .collect(),
        }
    }

    /// Reads an immutable window at the current or explicitly retained revision.
    #[phoxal::runtime::outputs::read(
        port = ports::WINDOW,
        project = Self::read_view,
        max_request_bytes = 512,
        max_response_bytes = 8_192
    )]
    fn window(&self, view: &WorldReadView, request: &WindowRequest) -> WindowResponse {
        let Some(requested) = request.requested.as_ref() else {
            return unavailable(
                WindowUnavailableReason::WorldUnavailable,
                view.current_revision,
            );
        };
        if requested.validate().is_err() || !view.available {
            return unavailable(
                WindowUnavailableReason::WorldUnavailable,
                view.current_revision,
            );
        }
        let window = if request.revision == 0 {
            view.current.as_ref()
        } else {
            view.history
                .iter()
                .find(|window| window.revision == request.revision)
        };
        let Some(window) = window else {
            return unavailable(
                WindowUnavailableReason::RevisionNotRetained,
                view.current_revision,
            );
        };
        let Some(covered) = window.covered.as_ref() else {
            return unavailable(
                WindowUnavailableReason::WorldUnavailable,
                view.current_revision,
            );
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
}

struct WorldReadView {
    available: bool,
    current_revision: u64,
    current: Option<GridWindow>,
    history: Vec<GridWindow>,
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
                &WorldInputs { pose: pose(20, 7) },
            )
            .expect("fresh pose");
        assert!(state.available);
        assert_eq!(state.revision, 1);
        let view = World.read_view(&state);
        let request = WindowRequest {
            requested: Some(Bounds {
                min_x_m: 0.2,
                min_y_m: 0.2,
                max_x_m: 0.8,
                max_y_m: 0.8,
            }),
            revision: 1,
        };
        let response = World.window(&view, &request);
        response.validate().expect("retained window response");
    }

    #[test]
    fn stale_pose_is_unavailable_instead_of_reusing_old_belief() {
        let state = WorldState::new(WorldConfig::default());
        let (state, _) = World
            .step(
                &context(0, 200, None),
                state,
                &WorldInputs { pose: pose(20, 7) },
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
