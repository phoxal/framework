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

fn world(at_ms: u64) -> Latest<WorldBelief> {
    Latest::from_sample(Sample::new(
        WorldBelief {
            frame_id: "odom".into(),
            x_m: 0.0,
            y_m: 0.0,
            yaw_rad: 0.0,
            confidence: 1.0,
            revision: 1,
            available: true,
            oldest_capture_time_nanos: Some(at_ms * 1_000_000),
        },
        ObservationStamp::new("world", ExecutionTime::from_nanos(at_ms * 1_000_000), None),
    ))
}

fn world_revision(at_ms: u64) -> Latest<WorldRevision> {
    Latest::from_sample(Sample::new(
        WorldRevision {
            revision: 1,
            available: true,
            oldest_capture_time_nanos: Some(at_ms * 1_000_000),
        },
        ObservationStamp::new("world", ExecutionTime::from_nanos(at_ms * 1_000_000), None),
    ))
}

fn motion(at_ms: u64) -> Latest<MotionStatus> {
    Latest::from_sample(Sample::new(
        MotionStatus {
            mode: crate::api::types::phoxal::motion::v1::ControlMode::Disarmed as i32,
            emergency_latched: false,
            selected_owner_id: None,
            protective_state_clear: false,
            stopped: true,
        },
        ObservationStamp::new("motion", ExecutionTime::from_nanos(at_ms * 1_000_000), None),
    ))
}

fn range(sensor_id: &str, distance_m: f64, at_ms: u64) -> Sample<RangeSample> {
    Sample::new(
        RangeSample {
            min_range_m: 0.0,
            max_range_m: 10.0,
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
        world_revision: world_revision(at_ms),
        motion: motion(at_ms),
        ranges: Samples::new(vec![range("front", 2.0, at_ms)]),
    }
}

#[test]
fn fresh_evidence_produces_clear_expiring_constraints() {
    let state = SafetyState::new(SafetyConfig {
        ranges: vec![required_range("front")],
        ..SafetyConfig::default()
    });
    let (state, _) = Safety
        .step(&context(0, 20, None), state, &clear_inputs(20))
        .expect("fresh evidence");
    assert_eq!(state.constraints.permission, Permission::Clear as i32);
    assert_eq!(state.status.sequence, 1);
    assert!(state.constraints.expires_at_nanos > state.constraints.valid_from_nanos);
}

#[test]
fn missing_world_or_motion_fails_closed() {
    let state = SafetyState::new(SafetyConfig {
        ranges: vec![required_range("front")],
        ..SafetyConfig::default()
    });
    let (state, _) = Safety
        .step(
            &context(0, 20, None),
            state,
            &SafetyInputs {
                world: Latest::unavailable(),
                world_revision: Latest::unavailable(),
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
    let state = SafetyState::new(SafetyConfig {
        ranges: vec![required_range("front")],
        ..SafetyConfig::default()
    });
    let (state, _) = Safety
        .step(
            &context(0, 20, None),
            state,
            &SafetyInputs {
                world: world(20),
                world_revision: world_revision(20),
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
                world_revision: world_revision(40),
                motion: motion(40),
                ranges: Samples::new(vec![range("front", 0.5, 40)]),
            },
        )
        .expect("midrange range");
    assert_eq!(state.constraints.permission, Permission::Limited as i32);
    assert_eq!(
        state.constraints.constraints[0].max_linear_speed_mps,
        Some(SafetyConfig::default().proximity_linear_limit_mps)
    );
}

#[test]
fn a_slow_range_capture_does_not_clear_a_stop_between_samples() {
    let state = SafetyState::new(SafetyConfig {
        ranges: vec![required_range("front")],
        ..SafetyConfig::default()
    });
    let mut inputs = clear_inputs(20);
    inputs.ranges = Samples::new(vec![range("front", 0.2, 20)]);
    let (state, _) = Safety.step(&context(0, 20, None), state, &inputs).unwrap();
    assert_eq!(state.constraints.permission, Permission::Stopped as i32);
    let mut inputs = clear_inputs(40);
    inputs.ranges = Samples::default();
    let (state, _) = Safety
        .step(&context(1, 40, Some(20)), state, &inputs)
        .unwrap();
    assert_eq!(state.constraints.permission, Permission::Stopped as i32);
}

#[test]
fn every_configured_range_source_must_remain_fresh_and_valid() {
    let config = SafetyConfig {
        ranges: vec![required_range("front"), required_range("rear")],
        ..SafetyConfig::default()
    };
    let mut state = SafetyState::new(config.clone());
    for (index, now, ranges, expected) in [
        (0, 0, vec![range("front", 2.0, 0)], Permission::Stopped),
        (1, 20, vec![range("rear", 2.0, 20)], Permission::Clear),
        (2, 40, vec![], Permission::Clear),
        (3, 120, vec![range("rear", 2.0, 120)], Permission::Stopped),
        (4, 140, vec![range("front", 2.0, 140)], Permission::Clear),
        (
            5,
            160,
            vec![range("front", f64::NAN, 160)],
            Permission::Stopped,
        ),
        (6, 180, vec![], Permission::Stopped),
        (7, 200, vec![range("front", 2.0, 200)], Permission::Clear),
    ] {
        let mut inputs = clear_inputs(now);
        inputs.ranges = Samples::new(ranges);
        (state, _) = Safety
            .step(&context(index, now, now.checked_sub(20)), state, &inputs)
            .unwrap();
        assert_eq!(state.constraints.permission, expected as i32, "at {now}ms");
        assert!(state.ranges.len() <= config.ranges.len());
    }
    let reset = SafetyState::new(config);
    assert!(reset.ranges.is_empty());
    assert_eq!(reset.constraints.permission, Permission::Stopped as i32);
}

#[test]
fn invalid_config_is_rejected_before_initialization() {
    let mut config = SafetyConfig {
        ranges: vec![required_range("front")],
        ..SafetyConfig::default()
    };
    config.ranges[0].protective_stop_distance_m = 2.0;
    assert!(RuntimeOwner::new(Safety, ExecutionTime::from_nanos(0), config).is_err());
}

#[test]
fn derived_world_capture_age_and_retained_ranges_bound_the_constraint_expiry() {
    let config = SafetyConfig {
        ranges: vec![required_range("front")],
        input_max_age_ms: 100,
        constraint_ttl_ms: 100,
        ..SafetyConfig::default()
    };
    let mut input = clear_inputs(90);
    let mut belief = input.world.value().unwrap().clone();
    belief.oldest_capture_time_nanos = Some(0);
    input.world = Latest::from_sample(Sample::new(
        belief,
        ObservationStamp::new("world", ExecutionTime::from_nanos(90_000_000), None),
    ));
    input.ranges = Samples::new(vec![range("front", 2.0, 10)]);
    let (state, _) = Safety
        .step(
            &context(0, 90, None),
            SafetyState::new(config.clone()),
            &input,
        )
        .unwrap();
    assert_eq!(state.constraints.permission, Permission::Clear as i32);
    assert_eq!(state.constraints.oldest_capture_time_nanos, Some(0));
    assert_eq!(state.constraints.expires_at_nanos, 100_000_000);
    for stale_revision in [false, true] {
        let mut input = clear_inputs(200);
        let stamp = ObservationStamp::new("world", ExecutionTime::from_nanos(200_000_000), None);
        if stale_revision {
            let mut value = *input.world_revision.value().unwrap();
            value.oldest_capture_time_nanos = Some(0);
            input.world_revision = Latest::from_sample(Sample::new(value, stamp));
        } else {
            let mut value = input.world.value().unwrap().clone();
            value.oldest_capture_time_nanos = Some(0);
            input.world = Latest::from_sample(Sample::new(value, stamp));
        }
        let (state, _) = Safety
            .step(
                &context(0, 200, None),
                SafetyState::new(config.clone()),
                &input,
            )
            .unwrap();
        assert_eq!(state.constraints.permission, Permission::Stopped as i32);
        assert_eq!(state.constraints.expires_at_nanos, 200_000_000);
    }
}

fn required_range(sensor_id: &str) -> crate::config::RangeRequirement {
    crate::config::RangeRequirement {
        sensor_id: sensor_id.into(),
        protective_stop_distance_m: 0.25,
        maximum_clear_distance_m: None,
        proximity_limit_distance_m: Some(0.6),
    }
}

#[test]
fn downward_range_requires_ground_inside_its_authored_distance_envelope() {
    let config = SafetyConfig {
        ranges: vec![crate::config::RangeRequirement {
            sensor_id: "ground".into(),
            protective_stop_distance_m: 0.08,
            maximum_clear_distance_m: Some(0.4),
            proximity_limit_distance_m: None,
        }],
        ..SafetyConfig::default()
    };
    validate_config(&config).unwrap();
    for (distance, permission) in [
        (0.28, Permission::Clear),
        (0.07, Permission::Stopped),
        (0.41, Permission::Stopped),
        (10.0, Permission::Stopped),
    ] {
        let mut input = clear_inputs(20);
        input.ranges = Samples::new(vec![range("ground", distance, 20)]);
        let (state, _) = Safety
            .step(
                &context(0, 20, None),
                SafetyState::new(config.clone()),
                &input,
            )
            .unwrap();
        assert_eq!(
            state.constraints.permission, permission as i32,
            "ground distance {distance}"
        );
    }
}
