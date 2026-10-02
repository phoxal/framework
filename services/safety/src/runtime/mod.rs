mod assessment;
use assessment::{assess_motion, assess_ranges, assess_world, is_stop_reason, observed_constraint};

use crate::config::{SafetyConfig, validate_config};
use crate::contract::MotionStatus;
use crate::contract::SafetyStatus;
#[cfg(test)]
use crate::contract::safety_api::Inputs as SafetyInputs;
use crate::contract::{Constraint, ConstraintReason, MotionConstraints, Permission};
use crate::contract::{WorldBelief, WorldRevision};
use crate::validation;
use phoxal::contracts::component::range::RangeSample;
#[cfg(test)]
use phoxal::runtime::input::Latest;
use phoxal::runtime::input::Samples;
use phoxal::runtime::{Context, ExecutionTime, Observation, ObservationStamp};
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
            permission: Permission::Stopped,
            constraints: vec![observed_constraint(ConstraintReason::WorldUnavailable, 0.0)],
            oldest_capture_time_nanos: None,
            valid_from_nanos: 0,
            expires_at_nanos: 0,
        };
        let status = SafetyStatus {
            protective_state_clear: false,
            sequence: 0,
            reasons: vec![ConstraintReason::WorldUnavailable],
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
pub struct Safety {
    state: SafetyState,
}

#[phoxal::runtime(contract = crate::contract::SafetyApi, period_ms = 20)]
impl Safety {
    #[init]
    fn new(config: SafetyConfig) -> phoxal::Result<Self> {
        validate_config(&config)?;
        Ok(Self {
            state: SafetyState::new(config),
        })
    }

    #[step]
    fn assess(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        let state = &mut self.state;
        state.sequence = state.sequence.saturating_add(1);
        let now = ctx.now();
        let now_nanos = now.as_nanos();
        let expires_at_nanos = now_nanos.saturating_add(
            ExecutionTime::from_nanos(state.config.constraint_ttl_ms.saturating_mul(1_000_000))
                .as_nanos(),
        );
        let mut constraints = Vec::new();

        assess_world(
            state,
            &ctx.world(),
            &ctx.world_revision(),
            now,
            &mut constraints,
        );
        assess_motion(state, &ctx.motion(), now, &mut constraints);
        assess_ranges(state, ctx.ranges(), now, &mut constraints);

        let permission = if constraints.iter().any(is_stop_reason) {
            Permission::Stopped
        } else if constraints.is_empty() {
            Permission::Clear
        } else {
            Permission::Limited
        };
        let oldest_capture_time_nanos =
            assessment::oldest_capture(state, &ctx.world(), &ctx.world_revision(), &ctx.motion());
        let expires_at_nanos = oldest_capture_time_nanos
            .map_or(expires_at_nanos, |capture| {
                expires_at_nanos.min(
                    capture.saturating_add(state.config.input_max_age_ms.saturating_mul(1_000_000)),
                )
            })
            .max(now_nanos);
        let reasons = constraints
            .iter()
            .map(|item| item.reason)
            .collect::<Vec<_>>();
        state.constraints = MotionConstraints {
            sequence: state.sequence,
            permission,
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
        Ok(())
    }

    /// Projects the expiring protective constraints consumed by Motion.
    #[publish(constraints)]
    fn constraints(&self) -> MotionConstraints {
        self.state.constraints.clone()
    }

    /// Projects safety availability and the reasons currently preventing a
    /// clear permission.
    #[publish(status)]
    fn status(&self) -> SafetyStatus {
        self.state.status.clone()
    }
}

#[cfg(test)]
mod tests;
