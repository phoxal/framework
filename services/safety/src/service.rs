//! The official safety Runtime.
//!
//! Safety owns the bounded fail-closed assessment between world evidence and
//! motion protection.  It consumes service-owned world, range, and motion
//! health messages and publishes one expiring constraints product.  The
//! Runtime has no background query loop, implicit component discovery, or
//! emergency-stop authority: motion remains the only final actuator owner.

use std::collections::BTreeMap;

use phoxal::runtime::input::{Latest, Samples};
use phoxal::runtime::{ExecutionTime, InitContext, Runtime, StepContext};
use phoxal_safety::{
    Constraint, ConstraintReason, MotionConstraints, MotionHealth, Permission, RangeObservation,
    SafetyStatus, WorldAssessment, ports,
};

const DEFAULT_INPUT_MAX_AGE_MS: u64 = 100;
const DEFAULT_CONSTRAINT_TTL_MS: u64 = 300;
const DEFAULT_PROTECTIVE_STOP_DISTANCE_M: f64 = 0.25;
const DEFAULT_PROXIMITY_LIMIT_DISTANCE_M: f64 = 0.60;
const DEFAULT_PROXIMITY_LINEAR_LIMIT_MPS: f64 = 0.15;
const MIN_LOCALIZATION_CONFIDENCE: f32 = 0.25;

/// Typed, validated policy for one Safety instance.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct SafetyConfig {
    /// Maximum age admitted for world, motion, and range evidence.
    #[serde(default = "default_input_max_age_ms")]
    pub input_max_age_ms: u64,
    /// Lifetime of each emitted protective constraints product.
    #[serde(default = "default_constraint_ttl_ms")]
    pub constraint_ttl_ms: u64,
    /// Distance at or below which a range observation requests a stop.
    #[serde(default = "default_protective_stop_distance_m")]
    pub protective_stop_distance_m: f64,
    /// Distance at or below which a range observation limits forward speed.
    #[serde(default = "default_proximity_limit_distance_m")]
    pub proximity_limit_distance_m: f64,
    /// Forward speed limit emitted for a proximity constraint.
    #[serde(default = "default_proximity_linear_limit_mps")]
    pub proximity_linear_limit_mps: f64,
    /// Require at least one valid range observation before allowing motion.
    #[serde(default)]
    pub require_range_data: bool,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            input_max_age_ms: DEFAULT_INPUT_MAX_AGE_MS,
            constraint_ttl_ms: DEFAULT_CONSTRAINT_TTL_MS,
            protective_stop_distance_m: DEFAULT_PROTECTIVE_STOP_DISTANCE_M,
            proximity_limit_distance_m: DEFAULT_PROXIMITY_LIMIT_DISTANCE_M,
            proximity_linear_limit_mps: DEFAULT_PROXIMITY_LINEAR_LIMIT_MPS,
            require_range_data: false,
        }
    }
}

const fn default_input_max_age_ms() -> u64 {
    DEFAULT_INPUT_MAX_AGE_MS
}

const fn default_constraint_ttl_ms() -> u64 {
    DEFAULT_CONSTRAINT_TTL_MS
}

const fn default_protective_stop_distance_m() -> f64 {
    DEFAULT_PROTECTIVE_STOP_DISTANCE_M
}

const fn default_proximity_limit_distance_m() -> f64 {
    DEFAULT_PROXIMITY_LIMIT_DISTANCE_M
}

const fn default_proximity_linear_limit_mps() -> f64 {
    DEFAULT_PROXIMITY_LINEAR_LIMIT_MPS
}

fn validate_config(config: &SafetyConfig) -> phoxal::Result<()> {
    if config.input_max_age_ms == 0 || config.constraint_ttl_ms == 0 {
        return Err(anyhow::anyhow!(
            "input_max_age_ms and constraint_ttl_ms must be positive"
        ));
    }
    for (value, field) in [
        (
            config.protective_stop_distance_m,
            "protective_stop_distance_m",
        ),
        (
            config.proximity_limit_distance_m,
            "proximity_limit_distance_m",
        ),
        (
            config.proximity_linear_limit_mps,
            "proximity_linear_limit_mps",
        ),
    ] {
        if !value.is_finite() || value <= 0.0 {
            return Err(anyhow::anyhow!("{field} must be finite and positive"));
        }
    }
    if config.protective_stop_distance_m > config.proximity_limit_distance_m {
        return Err(anyhow::anyhow!(
            "protective_stop_distance_m must not exceed proximity_limit_distance_m"
        ));
    }
    Ok(())
}

/// Private state retained by the serialized safety owner.
pub struct SafetyState {
    config: SafetyConfig,
    sequence: u64,
    constraints: MotionConstraints,
    status: SafetyStatus,
}

impl SafetyState {
    fn new(config: SafetyConfig) -> Self {
        let constraints = MotionConstraints {
            sequence: 0,
            permission: Permission::Stopped as i32,
            constraints: vec![observed_constraint(ConstraintReason::WorldUnavailable, 0.0)],
            valid_from_nanos: 0,
            expires_at_nanos: 0,
        };
        let status = SafetyStatus {
            protective_state_clear: false,
            sequence: 0,
            reasons: vec![ConstraintReason::WorldUnavailable as i32],
        };
        Self {
            config,
            sequence: 0,
            constraints,
            status,
        }
    }
}

/// One immutable input cut for Safety.
#[phoxal::runtime::inputs]
pub struct SafetyInputs {
    /// Latest world assessment from the world-owned integration boundary.
    #[phoxal::runtime::input(max_age_ms = 100)]
    pub world: Latest<WorldAssessment>,
    /// Latest motion health evidence.  This is health only, not actuator
    /// authority.
    #[phoxal::runtime::input(max_age_ms = 100)]
    pub motion: Latest<MotionHealth>,
    /// Bounded range observations retaining each sensor's capture stamp.
    #[phoxal::runtime::input(max_items = 64, max_bytes = 16_384)]
    pub ranges: Samples<RangeObservation>,
}

/// Safety emits only state projections.  Motion consumes the constraints
/// state and remains the sole service that emits final actuator intent.
pub type SafetyOutputs = ();

/// The official safety service implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Safety;

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Safety {
    type Config = SafetyConfig;
    type State = SafetyState;
    type Inputs = SafetyInputs;
    type Outputs = SafetyOutputs;

    fn validate_config(config: &Self::Config) -> phoxal::Result<()> {
        validate_config(config)
    }

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(SafetyState::new(config))
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        state.sequence = state.sequence.saturating_add(1);
        let now_nanos = ctx.now().as_nanos();
        let expires_at_nanos = now_nanos.saturating_add(
            ExecutionTime::from_nanos(state.config.constraint_ttl_ms.saturating_mul(1_000_000))
                .as_nanos(),
        );
        let mut constraints = Vec::new();

        assess_world(&state, inputs, ctx.now(), &mut constraints);
        assess_motion(&state, inputs, ctx.now(), &mut constraints);
        assess_ranges(&state, inputs, ctx.now(), &mut constraints);

        let permission = if constraints.iter().any(is_stop_reason) {
            Permission::Stopped
        } else if constraints.is_empty() {
            Permission::Clear
        } else {
            Permission::Limited
        };
        let reasons: Vec<i32> = constraints.iter().map(|item| item.reason).collect();
        state.constraints = MotionConstraints {
            sequence: state.sequence,
            permission: permission as i32,
            constraints,
            valid_from_nanos: now_nanos,
            expires_at_nanos,
        };
        state
            .constraints
            .validate()
            .map_err(|error| anyhow::anyhow!(error))?;
        state.status = SafetyStatus {
            protective_state_clear: permission == Permission::Clear,
            sequence: state.sequence,
            reasons,
        };
        state
            .status
            .validate()
            .map_err(|error| anyhow::anyhow!(error))?;
        Ok((state, ()))
    }
}

#[phoxal::runtime::outputs]
#[allow(
    dead_code,
    reason = "the collected projections are invoked by the transport runner"
)]
impl Safety {
    /// Projects the expiring protective constraints consumed by Motion.
    #[phoxal::runtime::outputs::state(
        port = ports::CONSTRAINTS,
        max_bytes = 4_096,
        bootstrap,
        on_change
    )]
    fn constraints(&self, state: &SafetyState) -> MotionConstraints {
        state.constraints.clone()
    }

    /// Projects safety availability and the reasons currently preventing a
    /// clear permission.
    #[phoxal::runtime::outputs::state(
        port = ports::STATUS,
        max_bytes = 1_024,
        bootstrap,
        on_change
    )]
    fn status(&self, state: &SafetyState) -> SafetyStatus {
        state.status.clone()
    }
}

fn assess_world(
    state: &SafetyState,
    inputs: &SafetyInputs,
    now: ExecutionTime,
    constraints: &mut Vec<Constraint>,
) {
    let Some(world) = fresh_latest(&inputs.world, now, state.config.input_max_age_ms) else {
        constraints.push(stop_constraint(ConstraintReason::WorldUnavailable));
        return;
    };
    if world.validate().is_err() || !world.localization_available {
        constraints.push(stop_constraint(ConstraintReason::WorldUnavailable));
        return;
    }
    if world.confidence < MIN_LOCALIZATION_CONFIDENCE {
        constraints.push(observed_constraint(
            ConstraintReason::LocalizationUncertain,
            f64::from(world.confidence),
        ));
    }
    if !world.map_available {
        constraints.push(stop_constraint(ConstraintReason::MapUnavailable));
    } else if !world.map_clear {
        constraints.push(stop_constraint(ConstraintReason::MapBlocked));
    }
}

fn assess_motion(
    state: &SafetyState,
    inputs: &SafetyInputs,
    now: ExecutionTime,
    constraints: &mut Vec<Constraint>,
) {
    let Some(motion) = fresh_latest(&inputs.motion, now, state.config.input_max_age_ms) else {
        constraints.push(stop_constraint(ConstraintReason::MotionUnavailable));
        return;
    };
    if !motion.available {
        constraints.push(stop_constraint(ConstraintReason::MotionUnavailable));
    }
    if motion.fault {
        constraints.push(stop_constraint(ConstraintReason::MotionFault));
    }
}

fn assess_ranges(
    state: &SafetyState,
    inputs: &SafetyInputs,
    now: ExecutionTime,
    constraints: &mut Vec<Constraint>,
) {
    let mut latest_by_sensor = BTreeMap::<String, (RangeObservation, ExecutionTime)>::new();
    for sample in inputs.ranges.items() {
        let observation = sample.payload();
        if observation.validate().is_err() {
            push_unique_constraint(constraints, stop_constraint(ConstraintReason::RangeFault));
            continue;
        }
        let Some(age) = now.checked_duration_since(sample.stamp().capture_time()) else {
            push_unique_constraint(constraints, stop_constraint(ConstraintReason::RangeFault));
            continue;
        };
        if age.as_millis() > state.config.input_max_age_ms {
            continue;
        }
        let replace = latest_by_sensor
            .get(&observation.sensor_id)
            .is_none_or(|(_, captured_at)| *captured_at < sample.stamp().capture_time());
        if replace {
            latest_by_sensor.insert(
                observation.sensor_id.clone(),
                (observation.clone(), sample.stamp().capture_time()),
            );
        }
    }

    if latest_by_sensor.is_empty() {
        if state.config.require_range_data {
            constraints.push(stop_constraint(ConstraintReason::RangeUnavailable));
        }
        return;
    }
    let mut nearest: Option<f64> = None;
    for (observation, _) in latest_by_sensor.values() {
        if !observation.valid {
            push_unique_constraint(constraints, stop_constraint(ConstraintReason::RangeFault));
            continue;
        }
        nearest = Some(nearest.map_or(observation.distance_m, |value| {
            value.min(observation.distance_m)
        }));
    }
    let Some(distance) = nearest else {
        push_unique_constraint(
            constraints,
            stop_constraint(ConstraintReason::RangeUnavailable),
        );
        return;
    };
    if distance <= state.config.protective_stop_distance_m {
        constraints.push(observed_constraint(
            ConstraintReason::ObstacleProximity,
            distance,
        ));
    } else if distance <= state.config.proximity_limit_distance_m {
        constraints.push(Constraint {
            reason: ConstraintReason::ObstacleProximity as i32,
            max_linear_speed_mps: Some(state.config.proximity_linear_limit_mps),
            max_angular_speed_radps: None,
            observed_value: Some(distance),
        });
    }
}

fn fresh_latest<T>(latest: &Latest<T>, now: ExecutionTime, max_age_ms: u64) -> Option<&T> {
    latest
        .is_fresh_at(now, Some(max_age_ms))
        .then(|| latest.value())
        .flatten()
}

fn stop_constraint(reason: ConstraintReason) -> Constraint {
    observed_constraint(reason, 0.0)
}

fn push_unique_constraint(constraints: &mut Vec<Constraint>, candidate: Constraint) {
    if let Some(existing) = constraints
        .iter_mut()
        .find(|existing| existing.reason == candidate.reason)
    {
        let candidate_is_stop =
            candidate.max_linear_speed_mps.is_none() && candidate.max_angular_speed_radps.is_none();
        let existing_is_limit =
            existing.max_linear_speed_mps.is_some() || existing.max_angular_speed_radps.is_some();
        if candidate_is_stop && existing_is_limit {
            *existing = candidate;
        }
        return;
    }
    constraints.push(candidate);
}

fn observed_constraint(reason: ConstraintReason, observed_value: f64) -> Constraint {
    Constraint {
        reason: reason as i32,
        max_linear_speed_mps: None,
        max_angular_speed_radps: None,
        observed_value: Some(observed_value),
    }
}

fn is_stop_reason(constraint: &Constraint) -> bool {
    !matches!(
        ConstraintReason::try_from(constraint.reason),
        Ok(ConstraintReason::ObstacleProximity)
            if constraint.max_linear_speed_mps.is_some()
                || constraint.max_angular_speed_radps.is_some()
    )
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

    fn world(at_ms: u64) -> Latest<WorldAssessment> {
        Latest::from_sample(Sample::new(
            WorldAssessment {
                localization_available: true,
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
                confidence: 1.0,
                map_available: true,
                map_clear: true,
            },
            ObservationStamp::new("world", ExecutionTime::from_nanos(at_ms * 1_000_000), None),
        ))
    }

    fn motion(at_ms: u64) -> Latest<MotionHealth> {
        Latest::from_sample(Sample::new(
            MotionHealth {
                available: true,
                fault: false,
            },
            ObservationStamp::new("motion", ExecutionTime::from_nanos(at_ms * 1_000_000), None),
        ))
    }

    fn range(sensor_id: &str, distance_m: f64, at_ms: u64) -> Sample<RangeObservation> {
        Sample::new(
            RangeObservation {
                sensor_id: sensor_id.to_owned(),
                distance_m,
                valid: true,
            },
            ObservationStamp::new(
                sensor_id,
                ExecutionTime::from_nanos(at_ms * 1_000_000),
                None,
            ),
        )
    }

    fn clear_inputs(at_ms: u64) -> SafetyInputs {
        SafetyInputs {
            world: world(at_ms),
            motion: motion(at_ms),
            ranges: Samples::new(vec![range("front", 2.0, at_ms)]),
        }
    }

    #[test]
    fn fresh_evidence_produces_clear_expiring_constraints() {
        let state = SafetyState::new(SafetyConfig::default());
        let (state, _) = Safety
            .step(&context(0, 20, None), state, &clear_inputs(20))
            .expect("fresh evidence");
        assert_eq!(state.constraints.permission, Permission::Clear as i32);
        assert_eq!(state.status.sequence, 1);
        assert!(state.constraints.expires_at_nanos > state.constraints.valid_from_nanos);
    }

    #[test]
    fn missing_world_or_motion_fails_closed() {
        let state = SafetyState::new(SafetyConfig::default());
        let (state, _) = Safety
            .step(
                &context(0, 20, None),
                state,
                &SafetyInputs {
                    world: Latest::unavailable(),
                    motion: Latest::unavailable(),
                    ranges: Samples::default(),
                },
            )
            .expect("missing evidence is a valid protective transition");
        assert_eq!(state.constraints.permission, Permission::Stopped as i32);
        assert!(!state.status.protective_state_clear);
        assert!(
            state
                .status
                .reasons
                .contains(&(ConstraintReason::WorldUnavailable as i32))
        );
        assert!(
            state
                .status
                .reasons
                .contains(&(ConstraintReason::MotionUnavailable as i32))
        );
    }

    #[test]
    fn close_range_stops_and_midrange_limits() {
        let state = SafetyState::new(SafetyConfig::default());
        let (state, _) = Safety
            .step(
                &context(0, 20, None),
                state,
                &SafetyInputs {
                    world: world(20),
                    motion: motion(20),
                    ranges: Samples::new(vec![range("front", 0.2, 20)]),
                },
            )
            .expect("close range");
        assert_eq!(state.constraints.permission, Permission::Stopped as i32);

        let (state, _) = Safety
            .step(
                &context(1, 40, Some(20)),
                state,
                &SafetyInputs {
                    world: world(40),
                    motion: motion(40),
                    ranges: Samples::new(vec![range("front", 0.5, 40)]),
                },
            )
            .expect("midrange range");
        assert_eq!(state.constraints.permission, Permission::Limited as i32);
        assert_eq!(
            state.constraints.constraints[0].max_linear_speed_mps,
            Some(DEFAULT_PROXIMITY_LINEAR_LIMIT_MPS)
        );
    }

    #[test]
    fn invalid_config_is_rejected_before_initialization() {
        let mut config = SafetyConfig::default();
        config.protective_stop_distance_m = config.proximity_limit_distance_m + 1.0;
        assert!(RuntimeOwner::new(Safety, ExecutionTime::from_nanos(0), config).is_err());
    }
}
