use crate::config::{NavigationConfig, validate_navigation_config};
use crate::contract::MapState;
use crate::contract::NavigationState;
#[cfg(test)]
use crate::contract::navigation_api::Inputs as NavigationInputs;
use crate::contract::{
    ApplyCommand, ApplyCommandResponse, GetGoalStatusRequest, GetGoalStatusResponse, GoalFinished,
    GoalOutcome, GoalTarget, Phase, RefusalReason, UnavailableReason,
};
use crate::validation;
use phoxal::contracts::robotics::OdometryState;
#[cfg(test)]
use phoxal::runtime::Sample;
#[cfg(test)]
use phoxal::runtime::input::{Commands, Latest};
use phoxal::runtime::{Context, ExecutionTime, Observation};
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
    unavailable_reasons: Vec<UnavailableReason>,
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

    fn set_active_goal(&mut self, goal: &crate::contract::StartGoal, map_revision: u64) {
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
#[derive(Debug)]
pub struct Navigation {
    planner: PlannerState,
}

#[phoxal::runtime(contract = crate::contract::NavigationApi, period_ms = 20)]
impl Navigation {
    #[init]
    fn new(config: NavigationConfig) -> phoxal::Result<Self> {
        validate_navigation_config(&config)?;
        Ok(Self {
            planner: PlannerState::new(config),
        })
    }

    /// Applies one navigation command at its merged dispatch position.
    ///
    /// Fresh availability and map facts are recomputed from the same frozen
    /// input cut at entry, exactly as the pre-dispatch refresh did, so the
    /// command observes the invocation's live facts rather than a stale
    /// retained copy.
    #[handle(apply_command)]
    fn apply(
        &mut self,
        ctx: &mut Context<'_, Self>,
        command: ApplyCommand,
    ) -> phoxal::Result<ApplyCommandResponse> {
        self.refresh_facts(ctx);
        let (response, terminal) = apply_command(&mut self.planner, &command);
        validation::command_response(&response).map_err(|error| anyhow::anyhow!(error))?;
        if let Some(finished) = terminal {
            validation::finished(&finished).map_err(|error| anyhow::anyhow!(error))?;
            self.planner.retain_terminal(finished.clone());
            ctx.emit_finished(finished)?;
        }
        Ok(response)
    }

    /// Answers one status query from the state at its merged position.
    #[handle(get_goal_status)]
    fn goal_status_query(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: GetGoalStatusRequest,
    ) -> phoxal::Result<GetGoalStatusResponse> {
        self.refresh_facts(ctx);
        let response = goal_status(&self.planner, &request);
        validation::status_response(&response).map_err(|error| anyhow::anyhow!(error))?;
        Ok(response)
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        self.refresh_facts(ctx);
        if self.planner.active_goal_id.is_some() && self.planner.unavailable() {
            if let Some(goal_id) = self.planner.active_goal_id.take() {
                let finished = GoalFinished {
                    goal_id,
                    outcome: GoalOutcome::Unavailable,
                    unavailable_reasons: self.planner.unavailable_reasons.clone(),
                };
                self.planner.clear_active();
                self.planner.retain_terminal(finished.clone());
                validation::finished(&finished).map_err(|error| anyhow::anyhow!(error))?;
                ctx.emit_finished(finished)?;
            }
        } else if self.planner.active_goal_id.is_some() {
            advance_search_or_follow(&mut self.planner, ctx.localization().value());
            if self.planner.phase == Phase::Idle
                && let Some(goal_id) = self.planner.active_goal_id.take()
            {
                let finished = GoalFinished {
                    goal_id,
                    outcome: GoalOutcome::Reached,
                    unavailable_reasons: Vec::new(),
                };
                self.planner.clear_active();
                self.planner.retain_terminal(finished.clone());
                validation::finished(&finished).map_err(|error| anyhow::anyhow!(error))?;
                ctx.emit_finished(finished)?;
            }
        }

        validation::state(&public_status(&self.planner)).map_err(|error| anyhow::anyhow!(error))?;
        Ok(())
    }

    /// Projects the private planner state to its public status port.
    #[publish(status)]
    fn status(&self) -> NavigationState {
        public_status(&self.planner)
    }

    /// Recomputes the fresh availability and map facts from this
    /// invocation's frozen input cut, under the contract's declared age
    /// bounds.
    fn refresh_facts(&mut self, ctx: &Context<'_, Self>) {
        let now = ctx.now();
        self.planner.unavailable_reasons =
            unavailable_reasons(&ctx.localization(), &ctx.map(), now);
        self.planner.map_revision = fresh_map_revision(&ctx.map(), now);
    }
}

fn goal_status(state: &PlannerState, request: &GetGoalStatusRequest) -> GetGoalStatusResponse {
    if validation::status_request(request).is_err() {
        return GetGoalStatusResponse::UnknownOrNoLongerRetained(
            crate::contract::GoalUnknownOrNoLongerRetained {
                goal_id: request.goal_id.clone(),
            },
        );
    }
    if state.active_goal_id.as_deref() == Some(request.goal_id.as_str()) {
        return GetGoalStatusResponse::Running(crate::contract::GoalRunning {
            goal_id: request.goal_id.clone(),
        });
    }
    if let Some(finished) = state
        .terminal_results
        .iter()
        .find(|finished| finished.goal_id == request.goal_id)
    {
        return GetGoalStatusResponse::Finished(finished.clone());
    }
    GetGoalStatusResponse::UnknownOrNoLongerRetained(
        crate::contract::GoalUnknownOrNoLongerRetained {
            goal_id: request.goal_id.clone(),
        },
    )
}

fn public_status(state: &PlannerState) -> NavigationState {
    NavigationState {
        phase: state.phase,
        active_goal_id: state.active_goal_id.clone(),
        map_revision: state.map_revision,
        unavailable_reasons: state.unavailable_reasons.clone(),
    }
}

fn unavailable_reasons(
    localization: &Observation<'_, OdometryState>,
    map: &Observation<'_, MapState>,
    now: ExecutionTime,
) -> Vec<UnavailableReason> {
    let mut reasons = Vec::with_capacity(2);
    let localization_ready = localization.is_fresh()
        && localization.value().is_some_and(|pose| {
            validation::odometry(pose).is_ok()
                && pose.available
                && validation::capture_is_fresh_at(
                    pose.oldest_capture_time_nanos,
                    now.as_nanos(),
                    LOCALIZATION_MAX_AGE_MS.saturating_mul(1_000_000),
                )
        });
    if !localization_ready {
        reasons.push(UnavailableReason::Localization);
    }
    let map_ready = map.is_fresh()
        && map.value().is_some_and(|revision| {
            validation::world_revision(revision).is_ok()
                && revision.available
                && validation::capture_is_fresh_at(
                    revision.oldest_capture_time_nanos,
                    now.as_nanos(),
                    MAP_MAX_AGE_MS.saturating_mul(1_000_000),
                )
        });
    if !map_ready {
        reasons.push(UnavailableReason::Map);
    }
    reasons
}

fn fresh_map_revision(map: &Observation<'_, MapState>, now: ExecutionTime) -> Option<u64> {
    map.is_fresh()
        .then(|| {
            map.value()
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

fn unavailable_response(reasons: &[UnavailableReason]) -> ApplyCommandResponse {
    ApplyCommandResponse::Refused(crate::contract::Refused {
        reason: RefusalReason::Unavailable,
        unavailable_reasons: reasons.to_vec(),
    })
}

fn refused(reason: RefusalReason) -> ApplyCommandResponse {
    ApplyCommandResponse::Refused(crate::contract::Refused {
        reason,
        unavailable_reasons: Vec::new(),
    })
}

fn accepted() -> ApplyCommandResponse {
    ApplyCommandResponse::Accepted
}

fn apply_command(
    state: &mut PlannerState,
    command: &ApplyCommand,
) -> (ApplyCommandResponse, Option<GoalFinished>) {
    if validation::command_request(command).is_err() {
        return (refused(RefusalReason::InvalidGoal), None);
    }
    match command {
        ApplyCommand::Start(goal) => {
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
                outcome: GoalOutcome::Replaced,
                unavailable_reasons: Vec::new(),
            });
            let map_revision = state.map_revision.unwrap_or_default();
            state.set_active_goal(goal, map_revision);
            (accepted(), replaced)
        }
        ApplyCommand::Cancel(cancel) => {
            let Some(active_goal_id) = state.active_goal_id.as_deref() else {
                return (refused(RefusalReason::UnknownGoal), None);
            };
            if active_goal_id != cancel.goal_id {
                return (refused(RefusalReason::UnknownGoal), None);
            }
            let finished = GoalFinished {
                goal_id: cancel.goal_id.clone(),
                outcome: GoalOutcome::Cancelled,
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
pub fn map_revision(revision: u64, at: ExecutionTime) -> Latest<MapState> {
    Latest::from_sample(Sample::new(
        MapState {
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

    use super::phoxal_runtime_navigation;
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

    fn owner() -> RuntimeOwner<phoxal_runtime_navigation::Adapter> {
        RuntimeOwner::new(phoxal_runtime_navigation::Adapter::new(), at(0), config())
            .expect("initialize navigation")
    }

    fn start(goal_id: &str, x_m: f64, y_m: f64) -> ApplyCommand {
        ApplyCommand::Start(crate::contract::StartGoal {
            goal_id: goal_id.to_owned(),
            target: Some(GoalTarget {
                frame_id: "map".to_owned(),
                x_m,
                y_m,
                final_heading_rad: None,
            }),
        })
    }

    fn cancel(goal_id: &str) -> ApplyCommand {
        ApplyCommand::Cancel(crate::contract::CancelGoal {
            goal_id: goal_id.to_owned(),
        })
    }

    fn query(goal_id: &str) -> GetGoalStatusRequest {
        GetGoalStatusRequest {
            goal_id: goal_id.to_owned(),
        }
    }

    fn inputs(commands: Vec<Command<ApplyCommand, ApplyCommandResponse>>) -> NavigationInputs {
        NavigationInputs {
            apply_command: Commands::new(commands),
            get_goal_status: Default::default(),
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
        let mut owner = owner();
        let command = Command::with_order(
            CommandOrder::new(1, 0, CommandId::new(1)),
            start("goal-a", 0.0, 0.0),
        );
        let accepted = owner
            .accept(&context(0, 0), &inputs(vec![command]))
            .expect("accept start");
        assert_eq!(accepted.outputs().apply_command_replies.len(), 1);
        assert_eq!(accepted.outputs().finished.len(), 0);

        let mut finished = Vec::new();
        for index in 1..=4 {
            let accepted = owner
                .accept(&context(index, index * 20_000_000), &inputs(Vec::new()))
                .expect("advance navigation");
            finished.extend(accepted.outputs().finished.iter().cloned());
        }
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].outcome, GoalOutcome::Reached);
    }

    /// The received-data trace across both operation endpoints: commands and
    /// queries interleave by their admitted `CommandOrder`, an exact order
    /// tie resolves command-first (declaration order never reorders received
    /// data), and each query observes the state at its own merge position.
    #[test]
    fn queries_merge_by_received_order_with_command_first_ties() {
        let mut owner = owner();
        let inputs = NavigationInputs {
            apply_command: Commands::new(vec![
                // Sequence 2: start goal-a.
                Command::with_order(
                    CommandOrder::new(1, 0, CommandId::new(2)),
                    start("goal-a", 10.0, 0.0),
                ),
                // Sequence 4: replace goal-a with goal-b.
                Command::with_order(
                    CommandOrder::new(1, 0, CommandId::new(4)),
                    start("goal-b", 10.0, 0.0),
                ),
            ]),
            get_goal_status: Commands::new(vec![
                // Sequence 1: ordered before the start of goal-a.
                Command::with_order(CommandOrder::new(1, 0, CommandId::new(1)), query("goal-a")),
                // Sequence 2: an exact tie with the start — the command
                // runs first.
                Command::with_order(CommandOrder::new(1, 0, CommandId::new(2)), query("goal-a")),
                // Sequence 3: between the start and the replacement.
                Command::with_order(CommandOrder::new(1, 0, CommandId::new(3)), query("goal-a")),
                // Sequence 5: after the replacement — the retained
                // terminal evidence answers.
                Command::with_order(CommandOrder::new(1, 0, CommandId::new(5)), query("goal-a")),
            ]),
            localization: inputs(Vec::new()).localization,
            map: inputs(Vec::new()).map,
        };
        let accepted = owner
            .accept(&context(0, 0), &inputs)
            .expect("accept the merged batch");

        let replies = &accepted.outputs().get_goal_status_replies;
        assert_eq!(replies.len(), 4, "every query received its reply");
        assert!(
            matches!(
                replies[0].response(),
                GetGoalStatusResponse::UnknownOrNoLongerRetained(_)
            ),
            "the query ordered before the start saw no active goal"
        );
        assert!(
            matches!(replies[1].response(), GetGoalStatusResponse::Running(_)),
            "the exact-order tie resolved command-first"
        );
        assert!(
            matches!(replies[2].response(), GetGoalStatusResponse::Running(_)),
            "the query between the two starts saw goal-a running"
        );
        assert!(
            matches!(
                replies[3].response(),
                GetGoalStatusResponse::Finished(finished)
                    if finished.outcome == GoalOutcome::Replaced
            ),
            "the query after the replacement saw the retained terminal evidence"
        );

        let outcomes = accepted
            .outputs()
            .finished
            .iter()
            .map(|finished| finished.outcome)
            .collect::<Vec<_>>();
        assert_eq!(outcomes, [GoalOutcome::Replaced]);
    }

    #[test]
    fn cancellation_during_search_is_one_terminal_event_and_no_replay() {
        let mut owner = owner();
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
            accepted.outputs().finished[0].outcome,
            GoalOutcome::Cancelled
        );
        let accepted = owner
            .accept(&context(2, 40_000_000), &inputs(Vec::new()))
            .expect("accept empty invocation");
        assert!(accepted.outputs().finished.is_empty());
    }

    #[test]
    fn unavailable_inputs_refuse_start_without_faulting_runtime() {
        let mut owner = owner();
        let unavailable = NavigationInputs {
            apply_command: Commands::new(vec![Command::new(
                CommandId::new(1),
                start("goal-a", 1.0, 0.0),
            )]),
            get_goal_status: Default::default(),
            localization: Latest::unavailable(),
            map: Latest::unavailable(),
        };
        let accepted = owner
            .accept(&context(0, 0), &unavailable)
            .expect("unavailability is a typed refusal");
        let response = accepted.outputs().apply_command_replies[0].response();
        assert!(matches!(
            response,
            ApplyCommandResponse::Refused(refused)
                if refused.reason == RefusalReason::Unavailable
        ));
        assert_eq!(owner.status(), phoxal::runtime::RuntimeStatus::Ready);
    }

    #[test]
    fn same_invocation_can_replace_then_cancel_without_replaying_events() {
        let mut owner = owner();
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
            .map(|finished| finished.outcome)
            .collect::<Vec<_>>();
        assert_eq!(outcomes, [GoalOutcome::Replaced, GoalOutcome::Cancelled]);
    }

    #[test]
    fn config_validates_positive_search_budget_and_tolerance() {
        let mut invalid = config();
        invalid.max_expansions_per_step = 0;
        assert!(
            phoxal::runtime::initialize(&phoxal_runtime_navigation::Adapter::new(), at(0), invalid)
                .is_err()
        );

        let mut invalid = config();
        invalid.goal_tolerance_m = f64::INFINITY;
        assert!(
            phoxal::runtime::initialize(&phoxal_runtime_navigation::Adapter::new(), at(0), invalid)
                .is_err()
        );

        let mut invalid = config();
        invalid.goal_tolerance_m = -0.01;
        assert!(
            phoxal::runtime::initialize(&phoxal_runtime_navigation::Adapter::new(), at(0), invalid)
                .is_err()
        );
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
            ),
            GetGoalStatusResponse::Running(_)
        ));

        state.clear_active();
        state.retain_terminal(GoalFinished {
            goal_id: "finished".to_owned(),
            outcome: GoalOutcome::Cancelled,
            unavailable_reasons: Vec::new(),
        });
        assert!(matches!(
            goal_status(
                &state,
                &GetGoalStatusRequest {
                    goal_id: "finished".to_owned()
                }
            ),
            GetGoalStatusResponse::Finished(_)
        ));
        assert!(matches!(
            goal_status(
                &state,
                &GetGoalStatusRequest {
                    goal_id: "evicted".to_owned()
                }
            ),
            GetGoalStatusResponse::UnknownOrNoLongerRetained(_)
        ));
    }
}
