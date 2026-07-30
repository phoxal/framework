//! Headless behavior execution over robot-root `behaviors/*.yaml` definitions.

use std::collections::{BTreeMap, VecDeque};

use anyhow::{Context, Result, bail};
use phoxal::api;
use phoxal::behavior::{BehaviorCatalog, BehaviorDefinition, Node, ValueType};
use phoxal::prelude::*;

pub struct Api {
    command: Subscriber<api::behavior::Command>,
    request: Subscriber<api::behavior::Request>,
    navigation_result: Subscriber<api::navigation::Result>,
    localization: Latest<api::localize::LocalizationState>,
    map_revision: Latest<api::map::Revision>,
    motion_state: Latest<api::motion::State>,
    safety_state: Latest<api::safety::State>,
    state: StatePublisher<api::behavior::State>,
    snapshot: StatePublisher<api::behavior::Snapshot>,
    event: StatePublisher<api::behavior::Event>,
    navigation_request: CommandPublisher<api::navigation::Request>,
    power_command: CommandPublisher<api::power::Command>,
}

#[derive(Clone)]
struct World {
    localization: Option<api::localize::LocalizationState>,
    map_ready: bool,
    manual_active: bool,
    safety_clear: bool,
}

struct Execution {
    id: String,
    root_id: String,
    args: BTreeMap<String, api::behavior::Value>,
    status: api::behavior::ExecutionStatus,
    started_at: RobotInstant,
    node_statuses: BTreeMap<String, api::behavior::NodeStatus>,
    node_started_at_ns: BTreeMap<String, u64>,
    retry_counts: BTreeMap<String, u32>,
    navigation_requests: BTreeMap<String, api::navigation::RequestId>,
    failure: Option<api::behavior::Failure>,
    completion_published: bool,
    active_request: Option<ActiveRequest>,
}

#[derive(Clone)]
struct ActiveRequest {
    request_id: api::behavior::RequestId,
    behavior_id: String,
    args: BTreeMap<String, api::behavior::Value>,
}

enum Effect {
    Navigate {
        request_id: api::navigation::RequestId,
        pose: api::navigation::Pose,
    },
    CancelNavigation(api::navigation::RequestId),
    Shutdown,
    CompleteRequest {
        request_id: api::behavior::RequestId,
        behavior_id: String,
        status: api::behavior::ExecutionStatus,
        failure: Option<api::behavior::Failure>,
    },
}

pub struct BehaviorServiceState {
    catalog: BehaviorCatalog,
    execution: Option<Execution>,
    queued: VecDeque<api::behavior::Request>,
    navigation_outcomes: BTreeMap<String, api::navigation::Outcome>,
    authoritative_root: Option<String>,
    next_execution: u64,
    next_event: u64,
}

#[phoxal::service(state = BehaviorServiceState, api = Api)]
pub struct BehaviorService;

impl Participant for BehaviorService {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let root = ctx.robot_root()?;
        let behavior_config = ctx.robot()?.behavior().cloned();
        // The topology reserves this participant before the behavior design is
        // complete. With no explicit manifest opt-in, do not inspect prototype
        // files or accept executable definitions: launch as a healthy, inert
        // boundary. This TODO can be removed only when the parked behavior-
        // orchestration plan is redesigned and intentionally enabled.
        let catalog = if behavior_config.is_some() {
            BehaviorCatalog::load(root)?
        } else {
            BehaviorCatalog::default()
        };
        if let Some(config) = &behavior_config {
            catalog.validate_root(&config.root)?;
        }
        Ok((
            BehaviorServiceState {
                catalog,
                execution: None,
                queued: VecDeque::new(),
                navigation_outcomes: BTreeMap::new(),
                authoritative_root: behavior_config
                    .filter(|config| config.autostart)
                    .map(|config| config.root),
                next_execution: 1,
                next_event: 1,
            },
            Api {
                command: ctx
                    .subscriber(api::topic::owner().behavior().command(), 32)
                    .await?,
                request: ctx
                    .subscriber(api::topic::owner().behavior().request(), 32)
                    .await?,
                navigation_result: ctx
                    .subscriber(api::topic::client().navigation().result(), 32)
                    .await?,
                localization: ctx.latest(api::topic::client().localize().state()).await?,
                map_revision: ctx.latest(api::topic::client().map().revision()).await?,
                motion_state: ctx.latest(api::topic::client().motion().state()).await?,
                safety_state: ctx.latest(api::topic::client().safety().state()).await?,
                state: ctx
                    .state_publisher(api::topic::owner().behavior().state())
                    .await?,
                snapshot: ctx
                    .state_publisher(api::topic::owner().behavior().snapshot())
                    .await?,
                event: ctx
                    .state_publisher(api::topic::owner().behavior().event())
                    .await?,
                navigation_request: ctx
                    .command_publisher(api::topic::client().navigation().request())
                    .await?,
                power_command: ctx
                    .command_publisher(api::topic::client().power().command())
                    .await?,
            },
        ))
    }

    async fn reset(
        &self,
        _ctx: ResetContext,
        _api: &Self::Api,
        state: &mut Self::State,
    ) -> Result<()> {
        state.execution = None;
        state.navigation_outcomes.clear();
        // Queued behavior requests and identity counters are host/operator
        // intent and process identity, not simulated-world projections.
        Ok(())
    }

    #[phoxal::step(hz = 20)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        while let Some(received) = api.navigation_result.try_recv() {
            state
                .navigation_outcomes
                .insert(received.body.request_id.value, received.body.outcome);
        }
        while let Some(received) = api.command.try_recv() {
            state.handle_command(api, received.body, step).await?;
        }
        while let Some(received) = api.request.try_recv() {
            state.handle_request(api, received.body, step).await?;
        }

        // Keep the terminal snapshot observable while idle, but release it once
        // queued work can advance. The completion event is emitted exactly once.
        let release_terminal = state.execution.as_ref().is_some_and(|execution| {
            execution.completion_published
                && (!state.queued.is_empty()
                    || state.authoritative_root.as_deref() != Some(execution.root_id.as_str()))
        });
        if release_terminal {
            state.execution = None;
        }

        if state.execution.is_none() {
            if let Some(root) = state.authoritative_root.clone() {
                state
                    .start_execution(api, root, BTreeMap::new(), step)
                    .await?;
            } else if let Some(request) = state.queued.pop_front() {
                state
                    .start_execution(api, request.behavior_id, request.args, step)
                    .await?;
            }
        }

        let motion_state = api.motion_state.latest();
        let world = World {
            localization: api.localization.latest(),
            map_ready: api.map_revision.latest().is_some(),
            manual_active: motion_state.as_ref().is_some_and(|state| {
                state.selected_source == Some(api::motion::Source::Manual)
                    && state.zero_reason.is_none()
            }),
            safety_clear: api.safety_state.latest().is_some_and(|state| state.clear),
        };
        if world.manual_active
            && state
                .execution
                .as_ref()
                .is_some_and(|execution| execution.active_request.is_some())
        {
            state.cancel_active_request(api, step).await?;
        }
        if !world.manual_active
            && state.execution.as_ref().is_some_and(|execution| {
                state.authoritative_root.as_deref() == Some(execution.root_id.as_str())
                    && execution.active_request.is_none()
            })
            && let Some(request) = state.queued.pop_front()
        {
            state.accept_request(api, request, step).await?;
        }
        let mut effects = Vec::new();
        let mut transitions = Vec::new();
        if let Some(execution) = state.execution.as_mut()
            && execution.status == api::behavior::ExecutionStatus::Running
        {
            let before = execution.node_statuses.clone();
            let definition = state
                .catalog
                .get(&execution.root_id)
                .expect("execution definition remains in immutable catalog");
            let bindings = execution.args.clone();
            let outcome = tick_node(
                &state.catalog,
                &definition.authored.root,
                &definition.authored.id,
                &bindings,
                execution,
                &world,
                &state.navigation_outcomes,
                step.now().ticks(),
                &mut effects,
            )?;
            for (path, status) in &execution.node_statuses {
                if before.get(path) != Some(status) {
                    transitions.push((path.clone(), *status));
                }
            }
            match outcome {
                api::behavior::NodeStatus::Succeeded => {
                    execution.status = api::behavior::ExecutionStatus::Succeeded;
                }
                api::behavior::NodeStatus::Failed => {
                    execution.status = api::behavior::ExecutionStatus::Failed;
                    if execution.failure.is_none() {
                        execution.failure = Some(failure(
                            api::behavior::FailureReason::ActionFailed,
                            "behavior root failed",
                            None,
                            None,
                        ));
                    }
                }
                _ => {}
            }
        }

        state.apply_effects(api, effects, step).await?;
        for (path, status) in transitions {
            state
                .publish_event(
                    api,
                    step,
                    api::behavior::EventKind::NodeTransition(status),
                    Some(path),
                    None,
                )
                .await?;
        }
        if state.execution.as_ref().is_some_and(|execution| {
            matches!(
                execution.status,
                api::behavior::ExecutionStatus::Succeeded
                    | api::behavior::ExecutionStatus::Failed
                    | api::behavior::ExecutionStatus::Cancelled
            ) && !execution.completion_published
        }) {
            state
                .publish_event(
                    api,
                    step,
                    api::behavior::EventKind::ExecutionCompleted,
                    None,
                    state
                        .execution
                        .as_ref()
                        .and_then(|execution| execution.failure.clone()),
                )
                .await?;
            if let Some(execution) = state.execution.as_mut() {
                execution.completion_published = true;
            }
        }
        state.publish_observation(api, step).await
    }
}

impl BehaviorServiceState {
    async fn handle_command(
        &mut self,
        api: &Api,
        command: api::behavior::Command,
        step: StepContext,
    ) -> Result<()> {
        if matches!(command, api::behavior::Command::Cancel)
            && self
                .execution
                .as_ref()
                .is_some_and(|execution| execution.active_request.is_some())
        {
            return self.cancel_active_request(api, step).await;
        }
        let Some(execution) = self.execution.as_mut() else {
            return Ok(());
        };
        let kind = match command {
            api::behavior::Command::Pause
                if execution.status == api::behavior::ExecutionStatus::Running =>
            {
                execution.status = api::behavior::ExecutionStatus::Paused;
                Some(api::behavior::EventKind::ExecutionPaused)
            }
            api::behavior::Command::Resume
                if execution.status == api::behavior::ExecutionStatus::Paused =>
            {
                execution.status = api::behavior::ExecutionStatus::Running;
                Some(api::behavior::EventKind::ExecutionResumed)
            }
            api::behavior::Command::Cancel
                if matches!(
                    execution.status,
                    api::behavior::ExecutionStatus::Running
                        | api::behavior::ExecutionStatus::Paused
                ) =>
            {
                for request_id in execution.navigation_requests.values() {
                    publish_navigation_cancel(api, step.now(), request_id.clone()).await?;
                }
                execution.status = api::behavior::ExecutionStatus::Cancelled;
                Some(api::behavior::EventKind::ExecutionCancelled)
            }
            _ => None,
        };
        if let Some(kind) = kind {
            self.publish_event(api, step, kind, None, None).await?;
        }
        Ok(())
    }

    async fn handle_request(
        &mut self,
        api: &Api,
        request: api::behavior::Request,
        step: StepContext,
    ) -> Result<()> {
        let invalid = if request.request_id.value.trim().is_empty() {
            Some("request_id must not be empty".to_string())
        } else if let Some(definition) = self.catalog.get(&request.behavior_id) {
            validate_runtime_args(definition, &request.args)
                .err()
                .map(|error| error.to_string())
        } else {
            Some(format!("unknown behavior '{}'", request.behavior_id))
        };
        if let Some(detail) = invalid {
            self.publish_request_event(
                api,
                step,
                &request,
                api::behavior::EventKind::RequestRejected(
                    api::behavior::FailureReason::InvalidArgument,
                ),
                Some(failure(
                    api::behavior::FailureReason::InvalidArgument,
                    &detail,
                    None,
                    None,
                )),
            )
            .await?;
            return Ok(());
        }

        if self.authoritative_root.is_some() && self.execution.is_none() {
            let root = self.authoritative_root.clone().expect("checked");
            self.start_execution(api, root, BTreeMap::new(), step)
                .await?;
        }

        if self.authoritative_root.is_none() {
            self.start_execution(api, request.behavior_id, request.args, step)
                .await?;
            return Ok(());
        }

        let active = self
            .execution
            .as_ref()
            .and_then(|execution| execution.active_request.as_ref())
            .is_some();
        if active {
            match request.conflict_policy {
                api::behavior::ConflictPolicy::Reject => {
                    self.publish_request_event(
                        api,
                        step,
                        &request,
                        api::behavior::EventKind::RequestRejected(
                            api::behavior::FailureReason::ResourceConflict,
                        ),
                        Some(failure(
                            api::behavior::FailureReason::ResourceConflict,
                            "the root already owns an active request",
                            None,
                            None,
                        )),
                    )
                    .await?;
                }
                api::behavior::ConflictPolicy::Queue => {
                    let position = self
                        .queued
                        .iter()
                        .position(|queued| queued.priority < request.priority)
                        .unwrap_or(self.queued.len());
                    self.queued.insert(position, request);
                }
                api::behavior::ConflictPolicy::Interrupt => {
                    self.cancel_active_request(api, step).await?;
                    self.accept_request(api, request, step).await?;
                }
            }
        } else {
            self.accept_request(api, request, step).await?;
        }
        Ok(())
    }

    async fn start_execution(
        &mut self,
        api: &Api,
        behavior_id: String,
        args: BTreeMap<String, api::behavior::Value>,
        step: StepContext,
    ) -> Result<()> {
        let definition = self
            .catalog
            .get(&behavior_id)
            .with_context(|| format!("behavior '{behavior_id}' is not in the validated catalog"))?;
        validate_runtime_args(definition, &args)?;
        let id = format!("execution-{}", self.next_execution);
        self.next_execution = self.next_execution.saturating_add(1);
        self.execution = Some(Execution {
            id,
            root_id: behavior_id,
            args,
            status: api::behavior::ExecutionStatus::Running,
            started_at: step.now(),
            node_statuses: BTreeMap::new(),
            node_started_at_ns: BTreeMap::new(),
            retry_counts: BTreeMap::new(),
            navigation_requests: BTreeMap::new(),
            failure: None,
            completion_published: false,
            active_request: None,
        });
        self.publish_event(
            api,
            step,
            api::behavior::EventKind::ExecutionStarted,
            None,
            None,
        )
        .await
    }

    async fn accept_request(
        &mut self,
        api: &Api,
        request: api::behavior::Request,
        step: StepContext,
    ) -> Result<()> {
        let active = ActiveRequest {
            request_id: request.request_id.clone(),
            behavior_id: request.behavior_id.clone(),
            args: request.args.clone(),
        };
        self.execution
            .as_mut()
            .context("authoritative root is unavailable")?
            .active_request = Some(active);
        self.publish_request_event(
            api,
            step,
            &request,
            api::behavior::EventKind::RequestAccepted,
            None,
        )
        .await
    }

    async fn cancel_active_request(&mut self, api: &Api, step: StepContext) -> Result<()> {
        let Some(execution) = self.execution.as_mut() else {
            return Ok(());
        };
        let Some(active) = execution.active_request.take() else {
            return Ok(());
        };
        let navigation = std::mem::take(&mut execution.navigation_requests);
        for request_id in navigation.into_values() {
            publish_navigation_cancel(api, step.now(), request_id).await?;
        }
        self.publish_request_outcome(
            api,
            step,
            active.request_id,
            active.behavior_id,
            api::behavior::ExecutionStatus::Cancelled,
            None,
        )
        .await
    }

    async fn apply_effects(
        &mut self,
        api: &Api,
        effects: Vec<Effect>,
        step: StepContext,
    ) -> Result<()> {
        for effect in effects {
            match effect {
                Effect::Navigate { request_id, pose } => {
                    api.navigation_request.send(api::navigation::Request {
                        request_id,
                        kind: api::navigation::RequestKind::GotoPose(pose),
                    })?;
                }
                Effect::CancelNavigation(request_id) => {
                    publish_navigation_cancel(api, step.now(), request_id).await?;
                }
                Effect::Shutdown => {
                    api.power_command.send(api::power::Command::Shutdown)?;
                }
                Effect::CompleteRequest {
                    request_id,
                    behavior_id,
                    status,
                    failure,
                } => {
                    if self.execution.as_ref().is_some_and(|execution| {
                        execution
                            .active_request
                            .as_ref()
                            .is_some_and(|active| active.request_id == request_id)
                    }) && let Some(execution) = self.execution.as_mut()
                    {
                        execution.active_request = None;
                        execution.navigation_requests.clear();
                        execution.failure = None;
                    }
                    self.publish_request_outcome(
                        api,
                        step,
                        request_id,
                        behavior_id,
                        status,
                        failure,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn publish_observation(&self, api: &Api, step: StepContext) -> Result<()> {
        let execution = self.execution.as_ref();
        let root = execution.and_then(|execution| self.catalog.get(&execution.root_id));
        let active_request = execution.and_then(|execution| execution.active_request.as_ref());
        let active_node_path = execution.and_then(|execution| {
            execution.node_statuses.iter().find_map(|(path, status)| {
                (*status == api::behavior::NodeStatus::Running).then(|| path.clone())
            })
        });
        api.state.publish(
            step.token(),
            api::behavior::State {
                execution_id: execution.map(|execution| execution.id.clone()),
                root_behavior_id: execution.map(|execution| execution.root_id.clone()),
                active_request_id: active_request.map(|active| active.request_id.clone()),
                active_behavior_id: active_request.map(|active| active.behavior_id.clone()),
                status: execution.map_or(api::behavior::ExecutionStatus::Idle, |execution| {
                    execution.status
                }),
                active_node_path: active_node_path.clone(),
                failure: execution.and_then(|execution| execution.failure.clone()),
            },
        )?;
        api.snapshot.publish(
            step.token(),
            api::behavior::Snapshot {
                execution_id: execution.map(|execution| execution.id.clone()),
                root: root.map(definition_ref),
                definition_stack: root.into_iter().map(definition_ref).collect(),
                active_request_id: active_request.map(|active| active.request_id.clone()),
                active_behavior_id: active_request.map(|active| active.behavior_id.clone()),
                status: execution.map_or(api::behavior::ExecutionStatus::Idle, |execution| {
                    execution.status
                }),
                node_statuses: execution
                    .map_or_else(BTreeMap::new, |execution| execution.node_statuses.clone()),
                active_node_path,
                blackboard: BTreeMap::new(),
                args: active_request.map_or_else(
                    || execution.map_or_else(BTreeMap::new, |execution| execution.args.clone()),
                    |active| active.args.clone(),
                ),
                started_at: execution.map(|execution| execution.started_at),
                failure: execution.and_then(|execution| execution.failure.clone()),
            },
        )?;
        Ok(())
    }

    async fn publish_event(
        &mut self,
        api: &Api,
        step: StepContext,
        kind: api::behavior::EventKind,
        node_path: Option<String>,
        failure: Option<api::behavior::Failure>,
    ) -> Result<()> {
        let execution = self.execution.as_ref();
        let definition = execution.and_then(|execution| self.catalog.get(&execution.root_id));
        let sequence = self.next_event;
        self.next_event = self.next_event.saturating_add(1);
        api.event.publish(
            step.token(),
            api::behavior::Event {
                sequence,
                execution_id: execution.map(|execution| execution.id.clone()),
                request_id: None,
                behavior_id: definition.map(|definition| definition.authored.id.clone()),
                content_hash: definition.map(|definition| definition.content_hash.clone()),
                node_path,
                kind,
                failure,
                participant_id: "behavior".to_string(),
            },
        )?;
        Ok(())
    }

    async fn publish_request_event(
        &mut self,
        api: &Api,
        step: StepContext,
        request: &api::behavior::Request,
        kind: api::behavior::EventKind,
        failure: Option<api::behavior::Failure>,
    ) -> Result<()> {
        self.publish_request_event_parts(
            api,
            step,
            request.request_id.clone(),
            request.behavior_id.clone(),
            kind,
            failure,
        )
        .await
    }

    async fn publish_request_outcome(
        &mut self,
        api: &Api,
        step: StepContext,
        request_id: api::behavior::RequestId,
        behavior_id: String,
        status: api::behavior::ExecutionStatus,
        failure: Option<api::behavior::Failure>,
    ) -> Result<()> {
        self.publish_request_event_parts(
            api,
            step,
            request_id,
            behavior_id,
            api::behavior::EventKind::RequestCompleted(status),
            failure,
        )
        .await
    }

    async fn publish_request_event_parts(
        &mut self,
        api: &Api,
        step: StepContext,
        request_id: api::behavior::RequestId,
        behavior_id: String,
        kind: api::behavior::EventKind,
        failure: Option<api::behavior::Failure>,
    ) -> Result<()> {
        let sequence = self.next_event;
        self.next_event = self.next_event.saturating_add(1);
        let execution_id = self
            .execution
            .as_ref()
            .map(|execution| execution.id.clone());
        let content_hash = self
            .catalog
            .get(&behavior_id)
            .map(|definition| definition.content_hash.clone());
        api.event.publish(
            step.token(),
            api::behavior::Event {
                sequence,
                execution_id,
                request_id: Some(request_id),
                behavior_id: Some(behavior_id),
                content_hash,
                node_path: None,
                kind,
                failure,
                participant_id: "behavior".to_string(),
            },
        )?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn tick_node(
    catalog: &BehaviorCatalog,
    node: &Node,
    parent_path: &str,
    bindings: &BTreeMap<String, api::behavior::Value>,
    execution: &mut Execution,
    world: &World,
    navigation_outcomes: &BTreeMap<String, api::navigation::Outcome>,
    now_ns: u64,
    effects: &mut Vec<Effect>,
) -> Result<api::behavior::NodeStatus> {
    let path = format!("{parent_path}/{}", node.id());
    if let Some(terminal) = execution.node_statuses.get(&path).copied()
        && matches!(
            terminal,
            api::behavior::NodeStatus::Succeeded | api::behavior::NodeStatus::Failed
        )
    {
        return Ok(terminal);
    }
    let status = match node {
        Node::Sequence { children, .. } => {
            let mut result = api::behavior::NodeStatus::Succeeded;
            for child in children {
                match tick_node(
                    catalog,
                    child,
                    &path,
                    bindings,
                    execution,
                    world,
                    navigation_outcomes,
                    now_ns,
                    effects,
                )? {
                    api::behavior::NodeStatus::Succeeded => {}
                    api::behavior::NodeStatus::Failed => {
                        result = api::behavior::NodeStatus::Failed;
                        break;
                    }
                    _ => {
                        result = api::behavior::NodeStatus::Running;
                        break;
                    }
                }
            }
            result
        }
        Node::Selector { children, .. } => {
            let mut result = api::behavior::NodeStatus::Failed;
            for child in children {
                match tick_node(
                    catalog,
                    child,
                    &path,
                    bindings,
                    execution,
                    world,
                    navigation_outcomes,
                    now_ns,
                    effects,
                )? {
                    api::behavior::NodeStatus::Failed => {}
                    api::behavior::NodeStatus::Succeeded => {
                        result = api::behavior::NodeStatus::Succeeded;
                        break;
                    }
                    _ => {
                        result = api::behavior::NodeStatus::Running;
                        break;
                    }
                }
            }
            result
        }
        Node::ReactiveSelector { children, .. } => {
            let mut result = api::behavior::NodeStatus::Failed;
            for child in children {
                let child_path = format!("{path}/{}", child.id());
                clear_reactive_status(&child_path, execution);
                match tick_node(
                    catalog,
                    child,
                    &path,
                    bindings,
                    execution,
                    world,
                    navigation_outcomes,
                    now_ns,
                    effects,
                )? {
                    api::behavior::NodeStatus::Failed => {}
                    api::behavior::NodeStatus::Succeeded => {
                        result = api::behavior::NodeStatus::Succeeded;
                        break;
                    }
                    _ => {
                        result = api::behavior::NodeStatus::Running;
                        break;
                    }
                }
            }
            result
        }
        Node::Condition {
            condition, args, ..
        } => {
            let passed = match condition.as_str() {
                "localization.confident" => {
                    let threshold = args
                        .get("min_confidence")
                        .and_then(|value| resolve_authored_value(value, bindings).ok())
                        .and_then(|value| value.as_f64())
                        .unwrap_or(0.0) as f32;
                    world
                        .localization
                        .as_ref()
                        .is_some_and(|localization| localization.confidence >= threshold)
                }
                "map.ready" => world.map_ready,
                "motion.manual_active" => world.manual_active,
                "safety.clear" => world.safety_clear,
                _ => false,
            };
            if passed {
                api::behavior::NodeStatus::Succeeded
            } else {
                api::behavior::NodeStatus::Failed
            }
        }
        Node::Action {
            action,
            args,
            timeout_ms,
            ..
        } => {
            let action_status = match action.as_str() {
                "navigation.goto_pose" => {
                    let request_id = execution
                        .navigation_requests
                        .entry(path.clone())
                        .or_insert_with(|| {
                            let request_id = api::navigation::RequestId {
                                value: format!("behavior-{}-{}", execution.id, sanitize(node.id())),
                            };
                            let pose = args
                                .get("pose")
                                .context("navigation.goto_pose requires pose")
                                .and_then(|value| resolve_authored_value(value, bindings))
                                .and_then(|value| {
                                    serde_json::from_value(value).map_err(anyhow::Error::from)
                                });
                            match pose {
                                Ok(pose) => effects.push(Effect::Navigate {
                                    request_id: request_id.clone(),
                                    pose,
                                }),
                                Err(error) => {
                                    execution.failure = Some(failure(
                                        api::behavior::FailureReason::InvalidArgument,
                                        &error.to_string(),
                                        Some(path.clone()),
                                        Some(action.clone()),
                                    ))
                                }
                            }
                            request_id
                        })
                        .clone();
                    match navigation_outcomes.get(&request_id.value) {
                        Some(api::navigation::Outcome::Succeeded) => {
                            api::behavior::NodeStatus::Succeeded
                        }
                        Some(api::navigation::Outcome::Cancelled) => {
                            execution.failure = Some(failure(
                                api::behavior::FailureReason::ActionCancelled,
                                "navigation action cancelled",
                                Some(path.clone()),
                                Some(action.clone()),
                            ));
                            api::behavior::NodeStatus::Failed
                        }
                        Some(api::navigation::Outcome::TimedOut) => {
                            execution.failure = Some(failure(
                                api::behavior::FailureReason::ActionTimedOut,
                                "navigation action timed out",
                                Some(path.clone()),
                                Some(action.clone()),
                            ));
                            api::behavior::NodeStatus::Failed
                        }
                        Some(api::navigation::Outcome::Failed(reason)) => {
                            execution.failure = Some(failure(
                                api::behavior::FailureReason::ActionFailed,
                                &format!("navigation failed: {reason:?}"),
                                Some(path.clone()),
                                Some(action.clone()),
                            ));
                            api::behavior::NodeStatus::Failed
                        }
                        Some(api::navigation::Outcome::Refused(reason)) => {
                            execution.failure = Some(failure(
                                api::behavior::FailureReason::ActionRefused,
                                &format!("navigation refused: {reason:?}"),
                                Some(path.clone()),
                                Some(action.clone()),
                            ));
                            api::behavior::NodeStatus::Failed
                        }
                        None if execution.failure.is_some() => api::behavior::NodeStatus::Failed,
                        None => api::behavior::NodeStatus::Running,
                    }
                }
                "navigation.stop" => {
                    for request_id in execution.navigation_requests.values() {
                        effects.push(Effect::CancelNavigation(request_id.clone()));
                    }
                    api::behavior::NodeStatus::Succeeded
                }
                "behavior.dispatch_request" => {
                    if let Some(active) = execution.active_request.clone() {
                        let requested = catalog.get(&active.behavior_id).with_context(|| {
                            format!("requested behavior '{}' disappeared", active.behavior_id)
                        })?;
                        let outcome = tick_node(
                            catalog,
                            &requested.authored.root,
                            &format!("{path}::request:{}", active.request_id.value),
                            &active.args,
                            execution,
                            world,
                            navigation_outcomes,
                            now_ns,
                            effects,
                        )?;
                        if matches!(
                            outcome,
                            api::behavior::NodeStatus::Succeeded
                                | api::behavior::NodeStatus::Failed
                        ) {
                            let status = if outcome == api::behavior::NodeStatus::Succeeded {
                                api::behavior::ExecutionStatus::Succeeded
                            } else {
                                api::behavior::ExecutionStatus::Failed
                            };
                            effects.push(Effect::CompleteRequest {
                                request_id: active.request_id,
                                behavior_id: active.behavior_id,
                                status,
                                failure: execution.failure.clone(),
                            });
                        }
                    }
                    api::behavior::NodeStatus::Running
                }
                "behavior.idle" => api::behavior::NodeStatus::Running,
                "host.shutdown" => {
                    effects.push(Effect::Shutdown);
                    api::behavior::NodeStatus::Succeeded
                }
                _ => api::behavior::NodeStatus::Failed,
            };
            if action_status == api::behavior::NodeStatus::Running
                && timeout_ms.is_some_and(|timeout_ms| {
                    let started = execution
                        .node_started_at_ns
                        .entry(path.clone())
                        .or_insert(now_ns);
                    now_ns.saturating_sub(*started) > timeout_ms.saturating_mul(1_000_000)
                })
            {
                if let Some(request_id) = execution.navigation_requests.get(&path) {
                    effects.push(Effect::CancelNavigation(request_id.clone()));
                }
                execution.failure = Some(failure(
                    api::behavior::FailureReason::ActionTimedOut,
                    "action timeout expired",
                    Some(path.clone()),
                    Some(action.clone()),
                ));
                api::behavior::NodeStatus::Failed
            } else {
                action_status
            }
        }
        Node::Wait { duration_ms, .. } => {
            let started = execution
                .node_started_at_ns
                .entry(path.clone())
                .or_insert(now_ns);
            if now_ns.saturating_sub(*started) >= duration_ms.saturating_mul(1_000_000) {
                api::behavior::NodeStatus::Succeeded
            } else {
                api::behavior::NodeStatus::Running
            }
        }
        Node::Timeout {
            timeout_ms, child, ..
        } => {
            let started = execution
                .node_started_at_ns
                .entry(path.clone())
                .or_insert(now_ns);
            if now_ns.saturating_sub(*started) > timeout_ms.saturating_mul(1_000_000) {
                for request_id in execution.navigation_requests.values() {
                    effects.push(Effect::CancelNavigation(request_id.clone()));
                }
                execution.failure = Some(failure(
                    api::behavior::FailureReason::ActionTimedOut,
                    "timeout node expired",
                    Some(path.clone()),
                    None,
                ));
                api::behavior::NodeStatus::Failed
            } else {
                tick_node(
                    catalog,
                    child,
                    &path,
                    bindings,
                    execution,
                    world,
                    navigation_outcomes,
                    now_ns,
                    effects,
                )?
            }
        }
        Node::Retry {
            attempts, child, ..
        } => {
            let outcome = tick_node(
                catalog,
                child,
                &path,
                bindings,
                execution,
                world,
                navigation_outcomes,
                now_ns,
                effects,
            )?;
            if outcome == api::behavior::NodeStatus::Failed {
                let count = execution.retry_counts.entry(path.clone()).or_default();
                *count = count.saturating_add(1);
                if *count < *attempts {
                    clear_subtree_status(&path, execution);
                    api::behavior::NodeStatus::Running
                } else {
                    api::behavior::NodeStatus::Failed
                }
            } else {
                outcome
            }
        }
        Node::Subtree { behavior, args, .. } => {
            let child = catalog
                .get(behavior)
                .with_context(|| format!("subtree '{behavior}' disappeared"))?;
            let child_bindings = resolve_subtree_bindings(child, args, bindings)?;
            tick_node(
                catalog,
                &child.authored.root,
                &format!("{path}::{behavior}"),
                &child_bindings,
                execution,
                world,
                navigation_outcomes,
                now_ns,
                effects,
            )?
        }
    };
    execution.node_statuses.insert(path, status);
    Ok(status)
}

fn clear_subtree_status(path: &str, execution: &mut Execution) {
    execution
        .node_statuses
        .retain(|node_path, _| !node_path.starts_with(path));
    execution
        .node_started_at_ns
        .retain(|node_path, _| !node_path.starts_with(path));
    execution
        .navigation_requests
        .retain(|node_path, _| !node_path.starts_with(path));
    execution.failure = None;
}

fn clear_reactive_status(path: &str, execution: &mut Execution) {
    execution
        .node_statuses
        .retain(|node_path, _| !node_path.starts_with(path));
}

fn validate_runtime_args(
    definition: &BehaviorDefinition,
    args: &BTreeMap<String, api::behavior::Value>,
) -> Result<()> {
    for name in args.keys() {
        if !definition.authored.inputs.contains_key(name) {
            bail!(
                "behavior '{}' does not declare arg '{name}'",
                definition.authored.id
            );
        }
    }
    for (name, kind) in &definition.authored.inputs {
        let value = args.get(name).with_context(|| {
            format!(
                "behavior '{}' requires arg '{name}'",
                definition.authored.id
            )
        })?;
        let matches = matches!(
            (kind, value),
            (ValueType::Bool, api::behavior::Value::Bool(_))
                | (ValueType::Integer, api::behavior::Value::Integer(_))
                | (ValueType::Number, api::behavior::Value::Number(_))
                | (ValueType::String, api::behavior::Value::String(_))
                | (ValueType::Pose, api::behavior::Value::Pose(_))
        );
        if !matches {
            bail!(
                "behavior '{}' arg '{name}' has wrong type",
                definition.authored.id
            );
        }
    }
    Ok(())
}

fn resolve_authored_value(
    value: &serde_json::Value,
    bindings: &BTreeMap<String, api::behavior::Value>,
) -> Result<serde_json::Value> {
    if let Some(reference) = value
        .as_str()
        .and_then(|value| value.strip_prefix("${input."))
        .and_then(|value| value.strip_suffix('}'))
    {
        return binding_to_json(
            bindings
                .get(reference)
                .with_context(|| format!("missing runtime input '{reference}'"))?,
        );
    }
    Ok(value.clone())
}

fn binding_to_json(value: &api::behavior::Value) -> Result<serde_json::Value> {
    Ok(match value {
        api::behavior::Value::Bool(value) => serde_json::Value::Bool(*value),
        api::behavior::Value::Integer(value) => serde_json::Value::from(*value),
        api::behavior::Value::Number(value) => serde_json::Value::from(*value),
        api::behavior::Value::String(value) => serde_json::Value::String(value.clone()),
        api::behavior::Value::Pose(value) => serde_json::to_value(value)?,
    })
}

fn resolve_subtree_bindings(
    definition: &BehaviorDefinition,
    authored: &BTreeMap<String, serde_json::Value>,
    parent: &BTreeMap<String, api::behavior::Value>,
) -> Result<BTreeMap<String, api::behavior::Value>> {
    definition
        .authored
        .inputs
        .iter()
        .map(|(name, kind)| {
            let value = authored
                .get(name)
                .with_context(|| format!("subtree input '{name}' is missing"))?;
            let resolved = resolve_authored_value(value, parent)?;
            Ok((name.clone(), json_to_binding(&resolved, *kind)?))
        })
        .collect()
}

fn json_to_binding(value: &serde_json::Value, kind: ValueType) -> Result<api::behavior::Value> {
    Ok(match kind {
        ValueType::Bool => {
            api::behavior::Value::Bool(value.as_bool().context("expected boolean subtree input")?)
        }
        ValueType::Integer => {
            api::behavior::Value::Integer(value.as_i64().context("expected integer subtree input")?)
        }
        ValueType::Number => {
            api::behavior::Value::Number(value.as_f64().context("expected numeric subtree input")?)
        }
        ValueType::String => api::behavior::Value::String(
            value
                .as_str()
                .context("expected string subtree input")?
                .to_string(),
        ),
        ValueType::Pose => api::behavior::Value::Pose(serde_json::from_value(value.clone())?),
    })
}

async fn publish_navigation_cancel(
    api: &Api,
    at: RobotInstant,
    target: api::navigation::RequestId,
) -> Result<()> {
    api.navigation_request.send(api::navigation::Request {
        request_id: api::navigation::RequestId {
            value: format!("cancel-{}-{}", target.value, at.ticks()),
        },
        kind: api::navigation::RequestKind::Cancel(target),
    })?;
    Ok(())
}

fn definition_ref(definition: &BehaviorDefinition) -> api::behavior::DefinitionRef {
    api::behavior::DefinitionRef {
        id: definition.authored.id.clone(),
        version: definition.authored.version.clone(),
        content_hash: definition.content_hash.clone(),
    }
}

fn failure(
    reason: api::behavior::FailureReason,
    detail: &str,
    node_path: Option<String>,
    action_id: Option<String>,
) -> api::behavior::Failure {
    api::behavior::Failure {
        reason,
        detail: Some(detail.to_string()),
        node_path,
        action_id,
    }
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use phoxal::bus::TimelineId;

    use super::*;

    fn catalog(yaml: &str) -> (tempfile::TempDir, BehaviorCatalog) {
        let root = tempfile::tempdir().expect("temp robot root");
        let behaviors = root.path().join("behaviors");
        std::fs::create_dir(&behaviors).expect("behavior directory");
        std::fs::write(behaviors.join("test.yaml"), yaml).expect("behavior definition");
        let catalog = BehaviorCatalog::load(root.path()).expect("valid catalog");
        (root, catalog)
    }

    fn execution(root_id: &str) -> Execution {
        Execution {
            id: "execution-test".to_string(),
            root_id: root_id.to_string(),
            args: BTreeMap::new(),
            status: api::behavior::ExecutionStatus::Running,
            started_at: RobotInstant::new(TimelineId::mint(), 0),
            node_statuses: BTreeMap::new(),
            node_started_at_ns: BTreeMap::new(),
            retry_counts: BTreeMap::new(),
            navigation_requests: BTreeMap::new(),
            failure: None,
            completion_published: false,
            active_request: None,
        }
    }

    fn tick(
        catalog: &BehaviorCatalog,
        execution: &mut Execution,
        now_ns: u64,
        navigation_outcomes: &BTreeMap<String, api::navigation::Outcome>,
        effects: &mut Vec<Effect>,
    ) -> api::behavior::NodeStatus {
        let definition = catalog.get(&execution.root_id).expect("definition");
        let bindings = execution.args.clone();
        tick_node(
            catalog,
            &definition.authored.root,
            &definition.authored.id,
            &bindings,
            execution,
            &World {
                localization: None,
                map_ready: false,
                manual_active: false,
                safety_clear: false,
            },
            navigation_outcomes,
            now_ns,
            effects,
        )
        .expect("tick")
    }

    #[test]
    fn wait_sequence_is_deterministic() {
        let (_root, catalog) = catalog(
            r#"schema: behavior/v0
id: test.wait
version: "1"
root:
  type: sequence
  id: root
  children:
    - type: wait
      id: pause
      duration_ms: 10
    - type: action
      id: shutdown
      action: host.shutdown
"#,
        );
        let mut execution = execution("test.wait");
        let outcomes = BTreeMap::new();
        let mut effects = Vec::new();
        assert_eq!(
            tick(&catalog, &mut execution, 1_000, &outcomes, &mut effects),
            api::behavior::NodeStatus::Running
        );
        assert!(effects.is_empty());
        assert_eq!(
            tick(
                &catalog,
                &mut execution,
                10_001_000,
                &outcomes,
                &mut effects
            ),
            api::behavior::NodeStatus::Succeeded
        );
        assert!(matches!(effects.as_slice(), [Effect::Shutdown]));
    }

    #[test]
    fn navigation_action_emits_once_then_consumes_typed_result() {
        let (_root, catalog) = catalog(
            r#"schema: behavior/v0
id: test.navigate
version: "1"
root:
  type: action
  id: goto
  action: navigation.goto_pose
  timeout_ms: 1000
  args:
    pose:
      x_m: 1.0
      y_m: 2.0
      yaw_rad: 0.5
"#,
        );
        let mut execution = execution("test.navigate");
        let mut effects = Vec::new();
        assert_eq!(
            tick(&catalog, &mut execution, 0, &BTreeMap::new(), &mut effects),
            api::behavior::NodeStatus::Running
        );
        let request_id = match effects.as_slice() {
            [Effect::Navigate { request_id, pose }]
                if pose.x_m == 1.0 && pose.y_m == 2.0 && pose.yaw_rad == Some(0.5) =>
            {
                request_id.value.clone()
            }
            _ => panic!("expected one navigation request"),
        };
        effects.clear();
        let outcomes = BTreeMap::from([(request_id, api::navigation::Outcome::Succeeded)]);
        assert_eq!(
            tick(&catalog, &mut execution, 1, &outcomes, &mut effects),
            api::behavior::NodeStatus::Succeeded
        );
        assert!(effects.is_empty());
    }

    #[test]
    fn retry_reexecutes_failed_child_until_attempt_budget_is_spent() {
        let (_root, catalog) = catalog(
            r#"schema: behavior/v0
id: test.retry
version: "1"
root:
  type: retry
  id: retry
  attempts: 2
  child:
    type: condition
    id: map
    condition: map.ready
"#,
        );
        let mut execution = execution("test.retry");
        let mut effects = Vec::new();
        assert_eq!(
            tick(&catalog, &mut execution, 0, &BTreeMap::new(), &mut effects),
            api::behavior::NodeStatus::Running
        );
        assert_eq!(
            tick(&catalog, &mut execution, 1, &BTreeMap::new(), &mut effects),
            api::behavior::NodeStatus::Failed
        );
    }

    #[test]
    fn runtime_args_reject_undeclared_values() {
        let (_root, catalog) = catalog(
            r#"schema: behavior/v0
id: test.args
version: "1"
inputs:
  enabled: bool
root:
  type: wait
  id: wait
  duration_ms: 1
"#,
        );
        let definition = catalog.get("test.args").expect("definition");
        let args = BTreeMap::from([
            ("enabled".to_string(), api::behavior::Value::Bool(true)),
            ("typo".to_string(), api::behavior::Value::Bool(true)),
        ]);
        assert!(validate_runtime_args(definition, &args).is_err());
    }

    #[test]
    fn authoritative_root_dispatches_correlated_request_without_being_replaced() {
        let root = tempfile::tempdir().expect("temp robot root");
        let behaviors = root.path().join("behaviors");
        std::fs::create_dir(&behaviors).expect("behavior directory");
        std::fs::write(
            behaviors.join("root.yaml"),
            "schema: behavior/v0\nid: system.root\nversion: 1\nroot: { type: action, id: dispatch, action: behavior.dispatch_request }\n",
        )
        .unwrap();
        std::fs::write(
            behaviors.join("job.yaml"),
            "schema: behavior/v0\nid: job\nversion: 1\nroot:\n  type: action\n  id: goto\n  action: navigation.goto_pose\n  timeout_ms: 1000\n  args: { pose: { x_m: 1.0, y_m: 0.0 } }\n",
        )
        .unwrap();
        let catalog = BehaviorCatalog::load(root.path()).unwrap();
        let mut execution = execution("system.root");
        execution.active_request = Some(ActiveRequest {
            request_id: api::behavior::RequestId {
                value: "request-1".to_string(),
            },
            behavior_id: "job".to_string(),
            args: BTreeMap::new(),
        });
        let mut effects = Vec::new();
        assert_eq!(
            tick(&catalog, &mut execution, 0, &BTreeMap::new(), &mut effects),
            api::behavior::NodeStatus::Running
        );
        assert_eq!(execution.root_id, "system.root");
        assert!(matches!(effects.as_slice(), [Effect::Navigate { .. }]));
        let navigation_id = execution
            .navigation_requests
            .values()
            .next()
            .unwrap()
            .value
            .clone();
        effects.clear();
        let outcomes = BTreeMap::from([(navigation_id, api::navigation::Outcome::Succeeded)]);
        assert_eq!(
            tick(&catalog, &mut execution, 1, &outcomes, &mut effects),
            api::behavior::NodeStatus::Running
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::CompleteRequest {
                status: api::behavior::ExecutionStatus::Succeeded,
                ..
            }]
        ));
    }

    #[test]
    fn subtree_runtime_binds_parent_input_into_child_action() {
        let root = tempfile::tempdir().unwrap();
        let behaviors = root.path().join("behaviors");
        std::fs::create_dir(&behaviors).unwrap();
        std::fs::write(
            behaviors.join("child.yaml"),
            "schema: behavior/v0\nid: child\nversion: 1\ninputs: { target: pose }\nroot:\n  type: action\n  id: goto\n  action: navigation.goto_pose\n  timeout_ms: 1000\n  args: { pose: '${input.target}' }\n",
        )
        .unwrap();
        std::fs::write(
            behaviors.join("parent.yaml"),
            "schema: behavior/v0\nid: parent\nversion: 1\ninputs: { destination: pose }\nroot:\n  type: subtree\n  id: child\n  behavior: child\n  args: { target: '${input.destination}' }\n",
        )
        .unwrap();
        let catalog = BehaviorCatalog::load(root.path()).unwrap();
        let mut execution = execution("parent");
        execution.args.insert(
            "destination".to_string(),
            api::behavior::Value::Pose(api::navigation::Pose {
                x_m: 2.0,
                y_m: 3.0,
                yaw_rad: None,
            }),
        );
        let mut effects = Vec::new();
        assert_eq!(
            tick(&catalog, &mut execution, 0, &BTreeMap::new(), &mut effects),
            api::behavior::NodeStatus::Running
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::Navigate { pose, .. }] if pose.x_m == 2.0 && pose.y_m == 3.0
        ));
    }
}
