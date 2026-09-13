//! Evaluation of retained protective evidence.

use super::*;

pub(super) fn assess_world(
    state: &SafetyState,
    inputs: &SafetyInputs,
    now: ExecutionTime,
    constraints: &mut Vec<Constraint>,
) {
    let Some(world) = fresh_latest(&inputs.world, now, state.config.input_max_age_ms) else {
        constraints.push(stop_constraint(ConstraintReason::WorldUnavailable));
        return;
    };
    if world.validate().is_err()
        || !world.available
        || !world.capture_is_fresh_at(
            now.as_nanos(),
            state.config.input_max_age_ms.saturating_mul(1_000_000),
        )
    {
        constraints.push(stop_constraint(ConstraintReason::WorldUnavailable));
        return;
    }
    if world.confidence < MIN_LOCALIZATION_CONFIDENCE {
        constraints.push(observed_constraint(
            ConstraintReason::LocalizationUncertain,
            f64::from(world.confidence),
        ));
    }
    let Some(revision) = fresh_latest(&inputs.world_revision, now, state.config.input_max_age_ms)
    else {
        constraints.push(stop_constraint(ConstraintReason::MapUnavailable));
        return;
    };
    if revision.validate().is_err()
        || !revision.available
        || !revision.capture_is_fresh_at(
            now.as_nanos(),
            state.config.input_max_age_ms.saturating_mul(1_000_000),
        )
    {
        constraints.push(stop_constraint(ConstraintReason::MapUnavailable));
    }
}

pub(super) fn assess_motion(
    state: &SafetyState,
    inputs: &SafetyInputs,
    now: ExecutionTime,
    constraints: &mut Vec<Constraint>,
) {
    let Some(motion) = fresh_latest(&inputs.motion, now, state.config.input_max_age_ms) else {
        constraints.push(stop_constraint(ConstraintReason::MotionUnavailable));
        return;
    };
    if motion.validate().is_err() {
        constraints.push(stop_constraint(ConstraintReason::MotionFault));
    }
}

pub(super) fn assess_ranges(
    state: &mut SafetyState,
    inputs: &SafetyInputs,
    now: ExecutionTime,
    constraints: &mut Vec<Constraint>,
) {
    for sample in inputs.ranges.items() {
        if !state
            .config
            .ranges
            .iter()
            .any(|range| range.sensor_id == sample.stamp().source())
        {
            push_unique_constraint(constraints, stop_constraint(ConstraintReason::RangeFault));
            continue;
        }
        if now
            .checked_duration_since(sample.stamp().capture_time())
            .is_none()
        {
            push_unique_constraint(constraints, stop_constraint(ConstraintReason::RangeFault));
            continue;
        }
        // Invalid newer readings replace older clear readings too. A fault
        // persists until a newer valid capture arrives or the source expires.
        let replace = state
            .ranges
            .get(sample.stamp().source())
            .is_none_or(|(_, stamp)| stamp.capture_time() < sample.stamp().capture_time());
        if replace {
            state.ranges.insert(
                sample.stamp().source().to_owned(),
                (*sample.payload(), sample.stamp().clone()),
            );
        }
    }
    for range in &state.config.ranges {
        let Some((observation, stamp)) = state.ranges.get(&range.sensor_id) else {
            push_unique_constraint(
                constraints,
                stop_constraint(ConstraintReason::RangeUnavailable),
            );
            continue;
        };
        if now
            .checked_duration_since(stamp.capture_time())
            .is_none_or(|age| {
                age.as_nanos() > state.config.input_max_age_ms.saturating_mul(1_000_000)
            })
        {
            push_unique_constraint(
                constraints,
                stop_constraint(ConstraintReason::RangeUnavailable),
            );
            continue;
        }
        if observation.validate().is_err() || !observation.valid {
            push_unique_constraint(constraints, stop_constraint(ConstraintReason::RangeFault));
            continue;
        }
        let distance = observation.distance_m;
        if range
            .maximum_clear_distance_m
            .is_some_and(|maximum| distance > maximum)
        {
            push_unique_constraint(
                constraints,
                observed_constraint(ConstraintReason::RangeUnavailable, distance),
            );
        } else if distance <= range.protective_stop_distance_m {
            push_unique_constraint(
                constraints,
                observed_constraint(ConstraintReason::ObstacleProximity, distance),
            );
        } else if range
            .proximity_limit_distance_m
            .is_some_and(|limit| distance <= limit)
        {
            push_unique_constraint(
                constraints,
                Constraint {
                    reason: ConstraintReason::ObstacleProximity as i32,
                    max_linear_speed_mps: Some(state.config.proximity_linear_limit_mps),
                    max_angular_speed_radps: None,
                    observed_value: Some(distance),
                },
            );
        }
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

pub(super) fn observed_constraint(reason: ConstraintReason, observed_value: f64) -> Constraint {
    Constraint {
        reason: reason as i32,
        max_linear_speed_mps: None,
        max_angular_speed_radps: None,
        observed_value: Some(observed_value),
    }
}

pub(super) fn is_stop_reason(constraint: &Constraint) -> bool {
    !matches!(
        ConstraintReason::try_from(constraint.reason),
        Ok(ConstraintReason::ObstacleProximity)
            if constraint.max_linear_speed_mps.is_some()
                || constraint.max_angular_speed_radps.is_some()
    )
}

/// Preserve the oldest piece of evidence used by this protective decision.
/// The availability assessments separately reject missing or invalid inputs.
pub(super) fn oldest_capture(state: &SafetyState, inputs: &SafetyInputs) -> Option<u64> {
    let mut oldest = inputs
        .world
        .value()?
        .oldest_capture_time_nanos?
        .min(inputs.world_revision.value()?.oldest_capture_time_nanos?)
        .min(inputs.motion.sample()?.stamp().capture_time().as_nanos());
    for range in &state.config.ranges {
        oldest = oldest.min(
            state
                .ranges
                .get(&range.sensor_id)?
                .1
                .capture_time()
                .as_nanos(),
        );
    }
    Some(oldest)
}
