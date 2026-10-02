//! Evaluation of retained protective evidence.

use super::*;

pub(super) fn assess_world(
    state: &SafetyState,
    world_view: &Observation<'_, WorldBelief>,
    revision_view: &Observation<'_, WorldRevision>,
    now: ExecutionTime,
    constraints: &mut Vec<Constraint>,
) {
    let Some(world) = world_view.fresh_within(state.config.input_max_age_ms) else {
        constraints.push(stop_constraint(ConstraintReason::WorldUnavailable));
        return;
    };
    if validation::world(world).is_err()
        || !world.available
        || !validation::capture_is_fresh_at(
            world.oldest_capture_time_nanos,
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
    let Some(revision) = revision_view.fresh_within(state.config.input_max_age_ms) else {
        constraints.push(stop_constraint(ConstraintReason::MapUnavailable));
        return;
    };
    if validation::world_revision(revision).is_err()
        || !revision.available
        || !validation::capture_is_fresh_at(
            revision.oldest_capture_time_nanos,
            now.as_nanos(),
            state.config.input_max_age_ms.saturating_mul(1_000_000),
        )
    {
        constraints.push(stop_constraint(ConstraintReason::MapUnavailable));
    }
}

pub(super) fn assess_motion(
    state: &SafetyState,
    motion_view: &Observation<'_, MotionStatus>,
    _now: ExecutionTime,
    constraints: &mut Vec<Constraint>,
) {
    let Some(motion) = motion_view.fresh_within(state.config.input_max_age_ms) else {
        constraints.push(stop_constraint(ConstraintReason::MotionUnavailable));
        return;
    };
    if validation::motion_status(motion).is_err() {
        constraints.push(stop_constraint(ConstraintReason::MotionFault));
    }
}

pub(super) fn assess_ranges(
    state: &mut SafetyState,
    ranges: &Samples<RangeSample>,
    now: ExecutionTime,
    constraints: &mut Vec<Constraint>,
) {
    for sample in ranges.items() {
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
                    reason: ConstraintReason::ObstacleProximity,
                    max_linear_speed_mps: Some(state.config.proximity_linear_limit_mps),
                    max_angular_speed_radps: None,
                    observed_value: Some(distance),
                },
            );
        }
    }
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
        reason,
        max_linear_speed_mps: None,
        max_angular_speed_radps: None,
        observed_value: Some(observed_value),
    }
}

pub(super) fn is_stop_reason(constraint: &Constraint) -> bool {
    !matches!(constraint.reason,
        ConstraintReason::ObstacleProximity
            if constraint.max_linear_speed_mps.is_some()
                || constraint.max_angular_speed_radps.is_some()
    )
}

/// Preserve the oldest piece of evidence used by this protective decision.
/// The availability assessments separately reject missing or invalid inputs.
pub(super) fn oldest_capture(
    state: &SafetyState,
    world: &Observation<'_, WorldBelief>,
    world_revision: &Observation<'_, WorldRevision>,
    motion: &Observation<'_, MotionStatus>,
) -> Option<u64> {
    let mut oldest = world
        .value()?
        .oldest_capture_time_nanos?
        .min(world_revision.value()?.oldest_capture_time_nanos?)
        .min(motion.sample()?.stamp().capture_time().as_nanos());
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
