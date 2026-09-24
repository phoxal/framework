use crate::api::__contracts::phoxal::kinematics::v1::OdometryState;
#[cfg(test)]
use crate::api::__contracts::phoxal::world::v1::WorldRevision;
use crate::api::navigation::v1::{
    ApplyCommandRequest, ApplyCommandResponse, GetGoalStatusRequest, GetGoalStatusResponse,
    GoalFinished, GoalOutcome, GoalTarget, NavigationState, Phase, RefusalReason,
    UnavailableReason, apply_command_request, apply_command_response, get_goal_status_response,
    navigation,
};
use crate::config::{NavigationConfig, validate_navigation_config};
use crate::inputs::NavigationInputs;
use crate::outputs::NavigationOutputs;
use crate::validation;
#[cfg(test)]
use phoxal::runtime::Sample;
#[cfg(test)]
use phoxal::runtime::input::{Commands, Latest};
use phoxal::runtime::{ExecutionTime, InitContext, Runtime, StepContext};
use std::collections::VecDeque;

const LOCALIZATION_MAX_AGE_MS: u64 = 100;

const MAP_MAX_AGE_MS: u64 = 100;

const SEARCH_STEPS: u32 = 3;

/// Private navigation state retained by the serialized compute owner.
#[derive(Debug)]
pub struct PlannerState {
    config: NavigationConfig,
    phase: Phase,
    active_goal_id: Option<String>,
    target: Option<GoalTarget>,
    map_revision: Option<u64>,
    unavailable_reasons: Vec<i32>,
    search_steps_remaining: u32,
    terminal_results: VecDeque<GoalFinished>,
}

impl PlannerState {
    fn new(config: NavigationConfig) -> Self {
        Self {
            config,
            phase: Phase::Idle,
            active_goal_id: None,
            target: None,
            map_revision: None,
            unavailable_reasons: Vec::new(),
            search_steps_remaining: 0,
            terminal_results: VecDeque::new(),
        }
    }

    fn unavailable(&self) -> bool {
        !self.unavailable_reasons.is_empty()
    }

    fn set_active_goal(&mut self, goal: &crate::api::navigation::v1::StartGoal, map_revision: u64) {
        self.phase = Phase::Searching;
        self.active_goal_id = Some(goal.goal_id.clone());
        self.target = goal.target.clone();
        self.map_revision = Some(map_revision);
        self.search_steps_remaining =
            SEARCH_STEPS.saturating_mul(self.config.max_expansions_per_step);
    }

    fn clear_active(&mut self) {
        self.phase = Phase::Idle;
        self.active_goal_id = None;
        self.target = None;
        self.search_steps_remaining = 0;
    }

    fn retain_terminal(&mut self, finished: GoalFinished) {
        if self.terminal_results.len() == validation::TERMINAL_RESULT_RETENTION {
            self.terminal_results.pop_front();
        }
        self.terminal_results.push_back(finished);
    }
}

/// The official navigation service implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Navigation;

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Navigation {
    type Config = NavigationConfig;
    type State = PlannerState;
    type Inputs = NavigationInputs;
    type Outputs = NavigationOutputs;

    fn validate_config(config: &Self::Config) -> phoxal::Result<()> {
        validate_navigation_config(config)
    }

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(PlannerState::new(config))
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        inputs
            .commands
            .validate_order()
            .map_err(|error| anyhow::anyhow!(error))?;
        inputs
            .status_calls
            .validate_order()
            .map_err(|error| anyhow::anyhow!(error))?;

        state.unavailable_reasons = unavailable_reasons(inputs, ctx.now());
        state.map_revision = fresh_map_revision(inputs, ctx.now());
        let mut outputs = NavigationOutputs::default();

        let mut command_index = 0;
        let mut status_index = 0;
        while command_index < inputs.commands.items().len()
            || status_index < inputs.status_calls.items().len()
        {
            let next_is_command = match (
                inputs.commands.items().get(command_index),
                inputs.status_calls.items().get(status_index),
            ) {
                (Some(command), Some(status)) => command.order() <= status.order(),
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if next_is_command {
                let command = &inputs.commands.items()[command_index];
                command_index += 1;
                let (response, terminal) = apply_command(&mut state, command.request());
                validation::command_response(&response).map_err(|error| anyhow::anyhow!(error))?;
                outputs.replies.push(command.reply(response));
                if let Some(finished) = terminal {
                    validation::finished(&finished).map_err(|error| anyhow::anyhow!(error))?;
                    state.retain_terminal(finished.clone());
                    outputs.finished.push(finished);
                }
            } else {
                let call = &inputs.status_calls.items()[status_index];
                status_index += 1;
                let response = goal_status(&state, call.request());
                validation::status_response(&response).map_err(|error| anyhow::anyhow!(error))?;
                outputs.status_replies.push(call.reply(response));
            }
        }

        if state.active_goal_id.is_some() && state.unavailable() {
            if let Some(goal_id) = state.active_goal_id.take() {
                let finished = GoalFinished {
                    goal_id,
                    outcome: GoalOutcome::Unavailable.into(),
                    unavailable_reasons: state.unavailable_reasons.clone(),
                };
                state.clear_active();
                state.retain_terminal(finished.clone());
                validation::finished(&finished).map_err(|error| anyhow::anyhow!(error))?;
                outputs.finished.push(finished);
            }
        } else if state.active_goal_id.is_some() {
            advance_search_or_follow(&mut state, inputs.localization.value());
            if state.phase == Phase::Idle
                && let Some(goal_id) = state.active_goal_id.take()
            {
                let finished = GoalFinished {
                    goal_id,
                    outcome: GoalOutcome::Reached.into(),
                    unavailable_reasons: Vec::new(),
                };
                state.clear_active();
                state.retain_terminal(finished.clone());
                validation::finished(&finished).map_err(|error| anyhow::anyhow!(error))?;
                outputs.finished.push(finished);
            }
        }

        validation::state(&public_status(&state)).map_err(|error| anyhow::anyhow!(error))?;
        Ok((state, outputs))
    }
}

#[phoxal::runtime::outputs]
#[allow(
    dead_code,
    reason = "the collected projections are invoked by the transport runner"
)]
impl Navigation {
    /// Projects the private planner state to its public status port.
    #[phoxal::runtime::outputs::state(
        port = navigation::methods::STATUS.__state_port(),
        max_bytes = 1_024,
        bootstrap
    )]
    fn status(&self, state: &PlannerState) -> NavigationState {
        public_status(state)
    }
}

fn goal_status(state: &PlannerState, request: &GetGoalStatusRequest) -> GetGoalStatusResponse {
    if validation::status_request(request).is_err() {
        return GetGoalStatusResponse {
            status: Some(get_goal_status_response::Status::UnknownOrNoLongerRetained(
                crate::api::navigation::v1::GoalUnknownOrNoLongerRetained {
                    goal_id: request.goal_id.clone(),
                },
            )),
        };
    }
    if state.active_goal_id.as_deref() == Some(request.goal_id.as_str()) {
        return GetGoalStatusResponse {
            status: Some(get_goal_status_response::Status::Running(
                crate::api::navigation::v1::GoalRunning {
                    goal_id: request.goal_id.clone(),
                },
            )),
        };
    }
    if let Some(finished) = state
        .terminal_results
        .iter()
        .find(|finished| finished.goal_id == request.goal_id)
    {
        return GetGoalStatusResponse {
            status: Some(get_goal_status_response::Status::Finished(finished.clone())),
        };
    }
    GetGoalStatusResponse {
        status: Some(get_goal_status_response::Status::UnknownOrNoLongerRetained(
            crate::api::navigation::v1::GoalUnknownOrNoLongerRetained {
                goal_id: request.goal_id.clone(),
            },
        )),
    }
}

fn public_status(state: &PlannerState) -> NavigationState {
    NavigationState {
        phase: state.phase.into(),
        active_goal_id: state.active_goal_id.clone(),
        map_revision: state.map_revision,
        unavailable_reasons: state.unavailable_reasons.clone(),
    }
}

fn unavailable_reasons(inputs: &NavigationInputs, now: ExecutionTime) -> Vec<i32> {
    let mut reasons = Vec::with_capacity(2);
    let localization_ready = inputs
        .localization
        .is_fresh_at(now, Some(LOCALIZATION_MAX_AGE_MS))
        && inputs.localization.value().is_some_and(|pose| {
            validation::odometry(pose).is_ok()
                && pose.available
                && validation::capture_is_fresh_at(
                    pose.oldest_capture_time_nanos,
                    now.as_nanos(),
                    LOCALIZATION_MAX_AGE_MS.saturating_mul(1_000_000),
                )
        });
    if !localization_ready {
        reasons.push(UnavailableReason::Localization.into());
    }
    let map_ready = inputs.map.is_fresh_at(now, Some(MAP_MAX_AGE_MS))
        && inputs.map.value().is_some_and(|revision| {
            validation::world_revision(revision).is_ok()
                && revision.available
                && validation::capture_is_fresh_at(
                    revision.oldest_capture_time_nanos,
                    now.as_nanos(),
                    MAP_MAX_AGE_MS.saturating_mul(1_000_000),
                )
        });
    if !map_ready {
        reasons.push(UnavailableReason::Map.into());
    }
    reasons
}

fn fresh_map_revision(inputs: &NavigationInputs, now: ExecutionTime) -> Option<u64> {
    inputs
        .map
        .is_fresh_at(now, Some(MAP_MAX_AGE_MS))
        .then(|| {
            inputs
                .map
                .value()
                .filter(|value| {
                    validation::world_revision(value).is_ok()
                        && value.available
                        && validation::capture_is_fresh_at(
                            value.oldest_capture_time_nanos,
                            now.as_nanos(),
                            MAP_MAX_AGE_MS.saturating_mul(1_000_000),
                        )
                })
                .map(|value| value.revision)
        })
        .flatten()
}

fn unavailable_response(reasons: &[i32]) -> ApplyCommandResponse {
    ApplyCommandResponse {
        decision: Some(apply_command_response::Decision::Refused(
            crate::api::navigation::v1::Refused {
                reason: RefusalReason::Unavailable.into(),
                unavailable_reasons: reasons.to_vec(),
            },
        )),
    }
}

fn refused(reason: RefusalReason) -> ApplyCommandResponse {
    ApplyCommandResponse {
        decision: Some(apply_command_response::Decision::Refused(
            crate::api::navigation::v1::Refused {
                reason: reason.into(),
                unavailable_reasons: Vec::new(),
            },
        )),
    }
}

fn accepted() -> ApplyCommandResponse {
    ApplyCommandResponse {
        decision: Some(apply_command_response::Decision::Accepted(
            crate::api::navigation::v1::Accepted {},
        )),
    }
}

fn apply_command(
    state: &mut PlannerState,
    request: &ApplyCommandRequest,
) -> (ApplyCommandResponse, Option<GoalFinished>) {
    if validation::command_request(request).is_err() {
        return (refused(RefusalReason::InvalidGoal), None);
    }
    let Some(command) = request.command.as_ref() else {
        return (refused(RefusalReason::InvalidGoal), None);
    };
    match command {
        apply_command_request::Command::Start(goal) => {
            if goal
                .target
                .as_ref()
                .is_none_or(|target| validation::goal_target(target).is_err())
                || goal.goal_id.is_empty()
                || goal.goal_id.len() > validation::MAX_ID_BYTES
            {
                return (refused(RefusalReason::InvalidGoal), None);
            }
            if state.unavailable() {
                return (unavailable_response(&state.unavailable_reasons), None);
            }
            let replaced = state.active_goal_id.take().map(|goal_id| GoalFinished {
                goal_id,
                outcome: GoalOutcome::Replaced.into(),
                unavailable_reasons: Vec::new(),
            });
            let map_revision = state.map_revision.unwrap_or_default();
            state.set_active_goal(goal, map_revision);
            (accepted(), replaced)
        }
        apply_command_request::Command::Cancel(cancel) => {
            let Some(active_goal_id) = state.active_goal_id.as_deref() else {
                return (refused(RefusalReason::UnknownGoal), None);
            };
            if active_goal_id != cancel.goal_id {
                return (refused(RefusalReason::UnknownGoal), None);
            }
            let finished = GoalFinished {
                goal_id: cancel.goal_id.clone(),
                outcome: GoalOutcome::Cancelled.into(),
                unavailable_reasons: Vec::new(),
            };
            state.clear_active();
            (accepted(), Some(finished))
        }
    }
}

fn advance_search_or_follow(state: &mut PlannerState, pose: Option<&OdometryState>) {
    match state.phase {
        Phase::Searching if state.search_steps_remaining > 0 => {
            state.search_steps_remaining = state
                .search_steps_remaining
                .saturating_sub(state.config.max_expansions_per_step);
            if state.search_steps_remaining == 0 {
                state.phase = Phase::Following;
            }
        }
        Phase::Following => {
            let Some(target) = state.target.as_ref() else {
                state.phase = Phase::Idle;
                return;
            };
            let Some(pose) = pose else {
                return;
            };
            let distance_squared =
                (target.x_m - pose.x_m).powi(2) + (target.y_m - pose.y_m).powi(2);
            if distance_squared.is_finite()
                && distance_squared <= state.config.goal_tolerance_m.powi(2)
            {
                state.phase = Phase::Idle;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
/// Construct a stamped kinematics odometry sample for direct Runtime tests and
/// adapters.
#[must_use]
pub fn odometry(value: OdometryState, at: ExecutionTime) -> Latest<OdometryState> {
    Latest::from_sample(Sample::new(
        value,
        phoxal::runtime::ObservationStamp::new("localization", at, None),
    ))
}

#[cfg(test)]
/// Construct a stamped world revision for direct Runtime tests and adapters.
#[must_use]
pub fn map_revision(revision: u64, at: ExecutionTime) -> Latest<WorldRevision> {
    Latest::from_sample(Sample::new(
        WorldRevision {
            revision,
            available: true,
            oldest_capture_time_nanos: Some(at.as_nanos()),
        },
        phoxal::runtime::ObservationStamp::new("map", at, Some(revision)),
    ))
}

#[cfg(test)]
mod tests {
    use phoxal::runtime::{
        Command, CommandId, CommandOrder, ExecutionDuration, RuntimeOwner, StepContext,
    };

    use super::*;

    fn at(nanos: u64) -> ExecutionTime {
        ExecutionTime::from_nanos(nanos)
    }

    fn config() -> NavigationConfig {
        NavigationConfig {
            max_expansions_per_step: 200,
            goal_tolerance_m: 0.15,
        }
    }

    fn start(goal_id: &str, x_m: f64, y_m: f64) -> ApplyCommandRequest {
        ApplyCommandRequest {
            command: Some(apply_command_request::Command::Start(
                crate::api::navigation::v1::StartGoal {
                    goal_id: goal_id.to_owned(),
                    target: Some(GoalTarget {
                        frame_id: "map".to_owned(),
                        x_m,
                        y_m,
                        final_heading_rad: None,
                    }),
                },
            )),
        }
    }

    fn cancel(goal_id: &str) -> ApplyCommandRequest {
        ApplyCommandRequest {
            command: Some(apply_command_request::Command::Cancel(
                crate::api::navigation::v1::CancelGoal {
                    goal_id: goal_id.to_owned(),
                },
            )),
        }
    }

    fn inputs(
        commands: Vec<Command<ApplyCommandRequest, ApplyCommandResponse>>,
    ) -> NavigationInputs {
        NavigationInputs {
            commands: Commands::new(commands),
            status_calls: Default::default(),
            localization: odometry(
                OdometryState {
                    x_m: 0.0,
                    y_m: 0.0,
                    yaw_rad: 0.0,
                    linear_x_mps: 0.0,
                    angular_z_radps: 0.0,
                    revision: 1,
                    available: true,
                    oldest_capture_time_nanos: Some(0),
                },
                at(0),
            ),
            map: map_revision(7, at(0)),
        }
    }

    fn context(index: u64, nanos: u64) -> StepContext {
        StepContext::new(
            at(nanos),
            ExecutionDuration::from_millis(20),
            ExecutionDuration::from_millis(20),
            0,
            index,
        )
    }

    #[test]
    fn direct_owner_accepts_ordered_commands_and_retains_reached_result() {
        let mut owner =
            RuntimeOwner::new(Navigation, at(0), config()).expect("initialize navigation");
        let command = Command::with_order(
            CommandOrder::new(1, 0, CommandId::new(1)),
            start("goal-a", 0.0, 0.0),
        );
        let accepted = owner
            .accept(&context(0, 0), &inputs(vec![command]))
            .expect("accept start");
        assert_eq!(accepted.outputs().replies.len(), 1);
        assert_eq!(accepted.outputs().finished.len(), 0);

        let mut finished = Vec::new();
        for index in 1..=4 {
            let accepted = owner
                .accept(&context(index, index * 20_000_000), &inputs(Vec::new()))
                .expect("advance navigation");
            finished.extend(accepted.outputs().finished.iter().cloned());
        }
        assert_eq!(finished.len(), 1);
        assert_eq!(
            GoalOutcome::try_from(finished[0].outcome),
            Ok(GoalOutcome::Reached)
        );
    }

    #[test]
    fn cancellation_during_search_is_one_terminal_event_and_no_replay() {
        let mut owner =
            RuntimeOwner::new(Navigation, at(0), config()).expect("initialize navigation");
        owner
            .accept(
                &context(0, 0),
                &inputs(vec![Command::new(
                    CommandId::new(1),
                    start("goal-a", 10.0, 0.0),
                )]),
            )
            .expect("accept start");
        let accepted = owner
            .accept(
                &context(1, 20_000_000),
                &inputs(vec![Command::new(CommandId::new(2), cancel("goal-a"))]),
            )
            .expect("accept cancellation");
        assert_eq!(accepted.outputs().finished.len(), 1);
        assert_eq!(
            GoalOutcome::try_from(accepted.outputs().finished[0].outcome),
            Ok(GoalOutcome::Cancelled)
        );
        let accepted = owner
            .accept(&context(2, 40_000_000), &inputs(Vec::new()))
            .expect("accept empty invocation");
        assert!(accepted.outputs().finished.is_empty());
    }

    #[test]
    fn unavailable_inputs_refuse_start_without_faulting_runtime() {
        let mut owner =
            RuntimeOwner::new(Navigation, at(0), config()).expect("initialize navigation");
        let unavailable = NavigationInputs {
            commands: Commands::new(vec![Command::new(
                CommandId::new(1),
                start("goal-a", 1.0, 0.0),
            )]),
            status_calls: Default::default(),
            localization: Latest::unavailable(),
            map: Latest::unavailable(),
        };
        let accepted = owner
            .accept(&context(0, 0), &unavailable)
            .expect("unavailability is a typed refusal");
        let response = accepted.outputs().replies[0].response();
        assert!(matches!(
            response.decision.as_ref(),
            Some(apply_command_response::Decision::Refused(refused))
                if refused.reason == RefusalReason::Unavailable as i32
        ));
        assert_eq!(owner.status(), phoxal::runtime::RuntimeStatus::Ready);
    }

    #[test]
    fn same_invocation_can_replace_then_cancel_without_replaying_events() {
        let mut owner =
            RuntimeOwner::new(Navigation, at(0), config()).expect("initialize navigation");
        let accepted = owner
            .accept(
                &context(0, 0),
                &inputs(vec![
                    Command::new(CommandId::new(1), start("goal-a", 10.0, 0.0)),
                    Command::new(CommandId::new(2), start("goal-b", 10.0, 0.0)),
                    Command::new(CommandId::new(3), cancel("goal-b")),
                ]),
            )
            .expect("accept complete command batch");
        let outcomes = accepted
            .outputs()
            .finished
            .iter()
            .map(|finished| GoalOutcome::try_from(finished.outcome).expect("known outcome"))
            .collect::<Vec<_>>();
        assert_eq!(outcomes, [GoalOutcome::Replaced, GoalOutcome::Cancelled]);
    }

    #[test]
    fn config_validates_positive_search_budget_and_tolerance() {
        let mut invalid = config();
        invalid.max_expansions_per_step = 0;
        assert!(phoxal::runtime::initialize(&Navigation, at(0), invalid).is_err());

        invalid = config();
        invalid.goal_tolerance_m = f64::INFINITY;
        assert!(phoxal::runtime::initialize(&Navigation, at(0), invalid).is_err());

        invalid = config();
        invalid.goal_tolerance_m = -0.01;
        assert!(phoxal::runtime::initialize(&Navigation, at(0), invalid).is_err());
    }

    #[test]
    fn goal_status_read_reconciles_running_finished_and_evicted_ids() {
        let mut state = PlannerState {
            phase: Phase::Searching,
            active_goal_id: Some("running".to_owned()),
            ..PlannerState::new(config())
        };
        assert!(matches!(
            goal_status(
                &state,
                &GetGoalStatusRequest {
                    goal_id: "running".to_owned()
                }
            )
            .status,
            Some(get_goal_status_response::Status::Running(_))
        ));

        state.clear_active();
        state.retain_terminal(GoalFinished {
            goal_id: "finished".to_owned(),
            outcome: GoalOutcome::Cancelled.into(),
            unavailable_reasons: Vec::new(),
        });
        assert!(matches!(
            goal_status(
                &state,
                &GetGoalStatusRequest {
                    goal_id: "finished".to_owned()
                }
            )
            .status,
            Some(get_goal_status_response::Status::Finished(_))
        ));
        assert!(matches!(
            goal_status(
                &state,
                &GetGoalStatusRequest {
                    goal_id: "evicted".to_owned()
                }
            )
            .status,
            Some(get_goal_status_response::Status::UnknownOrNoLongerRetained(
                _
            ))
        ));
    }
}
