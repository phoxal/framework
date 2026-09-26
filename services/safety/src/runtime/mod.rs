mod assessment;
use assessment::{assess_motion, assess_ranges, assess_world, is_stop_reason, observed_constraint};

use self::phoxal_provider::Inputs as SafetyInputs;
#[cfg(test)]
#[cfg(test)]
use crate::api::types::phoxal::motion::v1::MotionStatus;
use crate::api::types::phoxal::motion::v1::{
    Constraint, ConstraintReason, MotionConstraints, Permission,
};
use crate::api::types::phoxal::safety::v1::SafetyStatus;
#[cfg(test)]
use crate::api::types::phoxal::world::v1::{WorldBelief, WorldRevision};
use crate::config::{SafetyConfig, validate_config};
use crate::validation;
use phoxal::robotics::RangeSample;
use phoxal::runtime::input::Latest;
#[cfg(test)]
use phoxal::runtime::input::Samples;
use phoxal::runtime::{ExecutionTime, InitContext, ObservationStamp, Runtime, StepContext};
use std::collections::BTreeMap;

const MIN_LOCALIZATION_CONFIDENCE: f32 = 0.25;

/// Private state retained by the serialized safety owner.
pub struct SafetyState {
    config: SafetyConfig,
    sequence: u64,
    ranges: BTreeMap<String, (RangeSample, ObservationStamp)>,
    constraints: MotionConstraints,
    status: SafetyStatus,
}

impl SafetyState {
    fn new(config: SafetyConfig) -> Self {
        let constraints = MotionConstraints {
            sequence: 0,
            permission: Permission::Stopped as i32,
            constraints: vec![observed_constraint(ConstraintReason::WorldUnavailable, 0.0)],
            oldest_capture_time_nanos: None,
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
            ranges: BTreeMap::new(),
            sequence: 0,
            constraints,
            status,
        }
    }
}

/// The official safety service implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Safety;

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Safety {
    type Config = SafetyConfig;
    type State = SafetyState;

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
        assess_ranges(&mut state, inputs, ctx.now(), &mut constraints);

        let permission = if constraints.iter().any(is_stop_reason) {
            Permission::Stopped
        } else if constraints.is_empty() {
            Permission::Clear
        } else {
            Permission::Limited
        };
        let oldest_capture_time_nanos = assessment::oldest_capture(&state, inputs);
        let expires_at_nanos = oldest_capture_time_nanos
            .map_or(expires_at_nanos, |capture| {
                expires_at_nanos.min(
                    capture.saturating_add(state.config.input_max_age_ms.saturating_mul(1_000_000)),
                )
            })
            .max(now_nanos);
        let reasons: Vec<i32> = constraints.iter().map(|item| item.reason).collect();
        state.constraints = MotionConstraints {
            sequence: state.sequence,
            permission: permission as i32,
            constraints,
            oldest_capture_time_nanos,
            valid_from_nanos: now_nanos,
            expires_at_nanos,
        };
        validation::constraints(&state.constraints).map_err(|error| anyhow::anyhow!(error))?;
        state.status = SafetyStatus {
            protective_state_clear: permission == Permission::Clear,
            sequence: state.sequence,
            reasons,
        };
        validation::status(&state.status).map_err(|error| anyhow::anyhow!(error))?;
        Ok((state, Self::Outputs::default()))
    }
}

impl crate::api::projections::Projections for Safety {
    type State = SafetyState;

    /// Projects the expiring protective constraints consumed by Motion.
    fn constraints(&self, state: &SafetyState) -> MotionConstraints {
        state.constraints.clone()
    }

    /// Projects safety availability and the reasons currently preventing a
    /// clear permission.
    fn status(&self, state: &SafetyState) -> SafetyStatus {
        state.status.clone()
    }
}

#[cfg(test)]
mod tests;
