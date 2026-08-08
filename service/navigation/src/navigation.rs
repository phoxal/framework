//! `navigation` owns admitted start/cancel operations, planning, path
//! following, frontier proposal, progress, and terminal results.

use anyhow::Result;
use std::collections::VecDeque;
use std::time::Duration;

use phoxal::api;
use phoxal::bus::{ProducerId, QueryFailure};
use phoxal::prelude::*;

use crate::follower;
use crate::frontiers::OccupancyGrid;
use crate::planner;

const LOCALIZATION_STALE: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const RESULT_CACHE_CAPACITY: usize = 1024;

struct Active {
    operation_id: api::navigation::NavigationOperationId,
    requester: ProducerId,
    request_id: api::navigation::RequestId,
    path: api::navigation::Path,
    accepted_published: bool,
    cancel_requested: bool,
    started_at: RobotInstant,
}

struct CachedStart {
    requester: ProducerId,
    request_id: api::navigation::RequestId,
    response: api::navigation::StartResponse,
}

struct CompletedOperation {
    operation_id: api::navigation::NavigationOperationId,
    requester: ProducerId,
}

pub(crate) struct Api {
    localize: Subscriber<api::localize::LocalizationState>,
    map_revision: Subscriber<api::map::Revision>,
    map_submap: Querier<api::map::SubmapRequest, api::map::SubmapResponse>,
    state: StatePublisher<api::navigation::State>,
    progress: StatePublisher<api::navigation::Progress>,
    result: StatePublisher<api::navigation::Result>,
    candidate: StatePublisher<api::navigation::Candidate>,
}

pub(crate) struct NavigationState {
    server_producer: ProducerId,
    next_operation_sequence: u64,
    active: Option<Active>,
    last_localize: Option<Timed<api::localize::LocalizationState>>,
    last_map_revision: Option<Timed<api::map::Revision>>,
    starts: VecDeque<CachedStart>,
    completed: VecDeque<CompletedOperation>,
    last_time: Option<RobotInstant>,
}

impl NavigationState {
    fn new(server_producer: ProducerId) -> Self {
        Self {
            server_producer,
            next_operation_sequence: 0,
            active: None,
            last_localize: None,
            last_map_revision: None,
            starts: VecDeque::new(),
            completed: VecDeque::new(),
            last_time: None,
        }
    }

    fn reset(&mut self) {
        let producer = self.server_producer;
        let next_operation_sequence = self.next_operation_sequence;
        *self = Self::new(producer);
        // A timeline reset discards active operation state, but it does not
        // mint a new navigation producer. Keep the server-incarnation counter
        // monotonic so an operation id observed before the reset cannot be
        // confused with one admitted afterwards.
        self.next_operation_sequence = next_operation_sequence;
    }

    fn next_operation_id(&mut self) -> Option<api::navigation::NavigationOperationId> {
        let sequence = self.next_operation_sequence.checked_add(1)?;
        self.next_operation_sequence = sequence;
        Some(api::navigation::NavigationOperationId {
            producer: self.server_producer,
            sequence,
        })
    }

    fn cached_start(
        &self,
        requester: ProducerId,
        request_id: &api::navigation::RequestId,
    ) -> Option<api::navigation::StartResponse> {
        self.starts
            .iter()
            .find(|entry| entry.requester == requester && entry.request_id == *request_id)
            .map(|entry| entry.response.clone())
    }

    fn remember_start(
        &mut self,
        requester: ProducerId,
        request_id: api::navigation::RequestId,
        response: api::navigation::StartResponse,
    ) {
        if self.starts.len() == RESULT_CACHE_CAPACITY {
            self.starts.pop_front();
        }
        self.starts.push_back(CachedStart {
            requester,
            request_id,
            response,
        });
    }

    fn remember_completed(
        &mut self,
        operation_id: api::navigation::NavigationOperationId,
        requester: ProducerId,
    ) {
        if self.completed.len() == RESULT_CACHE_CAPACITY {
            self.completed.pop_front();
        }
        self.completed.push_back(CompletedOperation {
            operation_id,
            requester,
        });
    }

    fn completed_owner(
        &self,
        operation_id: api::navigation::NavigationOperationId,
    ) -> Option<ProducerId> {
        self.completed
            .iter()
            .find(|entry| entry.operation_id == operation_id)
            .map(|entry| entry.requester)
    }

    fn cancel(
        &mut self,
        requester: ProducerId,
        operation_id: api::navigation::NavigationOperationId,
    ) -> api::navigation::CancelResponse {
        if let Some(active) = self.active.as_mut() {
            if active.operation_id != operation_id {
                return api::navigation::CancelResponse::Refused(
                    api::navigation::RefusalReason::NotFound,
                );
            }
            if active.requester != requester {
                return api::navigation::CancelResponse::Refused(
                    api::navigation::RefusalReason::NotOwner,
                );
            }
            active.cancel_requested = true;
            return api::navigation::CancelResponse::Accepted;
        }
        match self.completed_owner(operation_id) {
            Some(owner) if owner == requester => api::navigation::CancelResponse::Accepted,
            Some(_) => {
                api::navigation::CancelResponse::Refused(api::navigation::RefusalReason::NotOwner)
            }
            None => {
                api::navigation::CancelResponse::Refused(api::navigation::RefusalReason::NotFound)
            }
        }
    }

    fn fresh_localization(
        &self,
        now: RobotInstant,
    ) -> Option<Timed<api::localize::LocalizationState>> {
        self.last_localize
            .as_ref()
            .filter(|sample| sample.fresh_within(now, LOCALIZATION_STALE))
            .cloned()
    }

    fn fresh_map_revision(&self, now: RobotInstant) -> Option<Timed<api::map::Revision>> {
        self.last_map_revision
            .as_ref()
            .filter(|sample| sample.fresh_within(now, LOCALIZATION_STALE))
            .cloned()
    }
}

#[phoxal::service(state = NavigationState, api = Api)]
pub(crate) struct Navigation;

impl Participant for Navigation {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        ctx.query(api::topic::owner().navigation().start(), Self::start)
            .await?;
        ctx.query(api::topic::owner().navigation().cancel(), Self::cancel)
            .await?;
        ctx.query(
            api::topic::owner().navigation().next_frontier(),
            Self::next_frontier,
        )
        .await?;
        Ok((
            NavigationState::new(ctx.producer()),
            Api {
                localize: ctx
                    .subscriber(api::topic::client().localize().state())
                    .await?,
                map_revision: ctx
                    .subscriber(api::topic::client().map().revision())
                    .await?,
                map_submap: ctx.querier(api::topic::client().map().submap()).await?,
                state: ctx
                    .state_publisher(api::topic::owner().navigation().state())
                    .await?,
                progress: ctx
                    .state_publisher(api::topic::owner().navigation().progress())
                    .await?,
                result: ctx
                    .state_publisher(api::topic::owner().navigation().result())
                    .await?,
                candidate: ctx
                    .state_publisher(api::topic::owner().navigation().candidate())
                    .await?,
            },
        ))
    }

    #[phoxal::step(hz = 20)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        let now = step.now();
        state.last_time = Some(now);
        while let Some(received) = api.localize.try_recv() {
            if let Some(at) = received.metadata.produced_exactly_at() {
                state.last_localize = Some(Timed::new(received.body, at));
            }
        }
        while let Some(received) = api.map_revision.try_recv() {
            if let Some(at) = received.metadata.produced_exactly_at() {
                state.last_map_revision = Some(Timed::new(received.body, at));
            }
        }

        let Some(active_snapshot) = state.active.as_ref() else {
            api.state
                .publish(&step.token, api::navigation::State::Idle)?;
            return Ok(());
        };
        let operation_id = active_snapshot.operation_id;
        let requester = active_snapshot.requester;
        let cancel_requested = active_snapshot.cancel_requested;
        let accepted_published = active_snapshot.accepted_published;
        let request_id = active_snapshot.request_id.clone();
        let path = active_snapshot.path.clone();
        let started_at = active_snapshot.started_at;
        let map_expected = path.map_revision;

        if cancel_requested {
            return state.abandon_active(api, step, api::navigation::Outcome::Cancelled);
        }
        if !accepted_published {
            if let Some(active) = state.active.as_mut() {
                active.accepted_published = true;
            }
            api.state
                .publish(&step.token, api::navigation::State::Accepted(operation_id))?;
            return Ok(());
        }

        let timed_out = now
            .duration_since(started_at)
            .is_ok_and(|elapsed| elapsed > REQUEST_TIMEOUT);
        if timed_out {
            return state.abandon_active(api, step, api::navigation::Outcome::TimedOut);
        }

        let map_revision = state.fresh_map_revision(now);
        if map_expected.is_some_and(|expected| {
            map_revision
                .as_ref()
                .is_none_or(|current| current.body.revision != expected)
        }) {
            let reason = if map_revision.is_some() {
                api::navigation::FailureReason::MapChanged
            } else {
                api::navigation::FailureReason::MapUnavailable
            };
            return state.abandon_active(api, step, api::navigation::Outcome::Failed(reason));
        }

        let Some(localization) = state.fresh_localization(now) else {
            return state.abandon_active(
                api,
                step,
                api::navigation::Outcome::Failed(
                    api::navigation::FailureReason::LocalizationUnavailable,
                ),
            );
        };

        let Some(output) = follower::pursue(&path, &localization.body) else {
            return state.abandon_active(
                api,
                step,
                api::navigation::Outcome::Failed(api::navigation::FailureReason::NoPath),
            );
        };

        api.state
            .publish(&step.token, api::navigation::State::Running(operation_id))?;
        api.progress.publish(
            &step.token,
            api::navigation::Progress {
                operation_id,
                request_id: request_id.clone(),
                distance_remaining_m: output.distance_remaining_m,
                path_index: output.target_index as u32,
            },
        )?;
        api.candidate.publish(
            &step.token,
            api::navigation::Candidate {
                operation_id,
                linear_x_mps: output.linear_x_mps,
                angular_z_radps: output.angular_z_radps,
            },
        )?;

        if output.finished {
            state.active = None;
            api.publish_zero_candidate(step, operation_id)?;
            state.publish_terminal(
                api,
                step,
                operation_id,
                requester,
                request_id,
                api::navigation::Outcome::Succeeded,
            )?;
        }
        Ok(())
    }

    fn reset(&self, _ctx: ResetContext, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        state.reset();
        Ok(())
    }
}

impl Navigation {
    async fn start(
        &self,
        _api: &Api,
        query: QueryContext,
        request: api::navigation::StartRequest,
        state: &mut NavigationState,
    ) -> QueryResult<api::navigation::StartResponse> {
        let requester = query.producer();
        if !request.request_id.is_valid() {
            return Ok(api::navigation::StartResponse::Refused(
                api::navigation::RefusalReason::InvalidRequest,
            ));
        }
        if let Some(response) = state.cached_start(requester, &request.request_id) {
            return Ok(response);
        }
        if state.active.is_some() {
            return Ok(api::navigation::StartResponse::Refused(
                api::navigation::RefusalReason::Busy,
            ));
        }
        let Some(now) = state.last_time else {
            return Ok(api::navigation::StartResponse::Refused(
                api::navigation::RefusalReason::Unavailable,
            ));
        };
        let Some(localization) = state.fresh_localization(now) else {
            return Ok(api::navigation::StartResponse::Refused(
                api::navigation::RefusalReason::Unavailable,
            ));
        };
        let Some(revision) = state.fresh_map_revision(now) else {
            return Ok(api::navigation::StartResponse::Refused(
                api::navigation::RefusalReason::Unavailable,
            ));
        };
        let Some(planning_extent) = planner::planning_extent(revision.body.resolution_m) else {
            return Ok(api::navigation::StartResponse::Refused(
                api::navigation::RefusalReason::Unavailable,
            ));
        };
        let path = match request.kind {
            api::navigation::StartKind::GotoPose(goal) => planner::straight_line(
                &localization.body,
                &goal,
                Some(revision.body.revision),
                planning_extent,
            ),
            api::navigation::StartKind::FollowPath(path) => {
                (planner::valid_path(&path, planning_extent)
                    && path.map_revision == Some(revision.body.revision))
                .then_some(path)
            }
        };
        let Some(path) = path else {
            return Ok(api::navigation::StartResponse::Refused(
                api::navigation::RefusalReason::InvalidRequest,
            ));
        };

        let Some(operation_id) = state.next_operation_id() else {
            return Ok(api::navigation::StartResponse::Refused(
                api::navigation::RefusalReason::Unavailable,
            ));
        };
        state.active = Some(Active {
            operation_id,
            requester,
            request_id: request.request_id.clone(),
            path,
            accepted_published: false,
            cancel_requested: false,
            started_at: now,
        });
        let response = api::navigation::StartResponse::Accepted { operation_id };
        state.remember_start(requester, request.request_id, response.clone());
        Ok(response)
    }

    async fn cancel(
        &self,
        _api: &Api,
        query: QueryContext,
        request: api::navigation::CancelRequest,
        state: &mut NavigationState,
    ) -> QueryResult<api::navigation::CancelResponse> {
        Ok(state.cancel(query.producer(), request.operation_id))
    }

    async fn next_frontier(
        &self,
        api: &Api,
        _query: QueryContext,
        request: api::navigation::FrontierRequest,
        state: &mut NavigationState,
    ) -> QueryResult<api::navigation::FrontierResponse> {
        let Some(now) = state.last_time else {
            return Err(QueryFailure::unavailable("no step has run yet"));
        };
        let Some(localization) = state.fresh_localization(now) else {
            return Err(QueryFailure::unavailable(
                "localization is unavailable or stale",
            ));
        };
        let Some(revision) = state.fresh_map_revision(now) else {
            return Err(QueryFailure::unavailable(
                "map revision is unavailable or stale",
            ));
        };
        if request
            .map_revision
            .is_some_and(|expected| expected != revision.body.revision)
        {
            return Ok(api::navigation::FrontierResponse {
                frontier: None,
                map_revision: Some(revision.body.revision),
            });
        }
        let extent = f64::from(revision.body.resolution_m) * 128.0;
        let response = api
            .map_submap
            .query(api::map::SubmapRequest {
                min_x_m: 0.0,
                min_y_m: 0.0,
                max_x_m: extent,
                max_y_m: extent,
            })
            .await
            .map_err(|error| QueryFailure::unavailable(error.to_string()))?;
        let robot_xy_m = (localization.body.x_m, localization.body.y_m);
        let frontier = OccupancyGrid::from_submap(response).and_then(|grid| {
            grid.score_frontiers(grid.detect_frontiers(), robot_xy_m)
                .into_iter()
                .next()
        });
        Ok(api::navigation::FrontierResponse {
            frontier,
            map_revision: Some(revision.body.revision),
        })
    }
}

impl Api {
    fn publish_result(
        &self,
        step: StepContext,
        operation_id: api::navigation::NavigationOperationId,
        request_id: api::navigation::RequestId,
        outcome: api::navigation::Outcome,
    ) -> Result<()> {
        self.result.publish(
            &step.token,
            api::navigation::Result {
                operation_id,
                request_id,
                outcome,
            },
        )?;
        Ok(())
    }

    fn publish_zero_candidate(
        &self,
        step: StepContext,
        operation_id: api::navigation::NavigationOperationId,
    ) -> Result<()> {
        self.candidate.publish(
            &step.token,
            api::navigation::Candidate {
                operation_id,
                linear_x_mps: 0.0,
                angular_z_radps: 0.0,
            },
        )?;
        Ok(())
    }
}

impl NavigationState {
    fn abandon_active(
        &mut self,
        api: &Api,
        step: StepContext,
        outcome: api::navigation::Outcome,
    ) -> Result<()> {
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        api.publish_zero_candidate(step, active.operation_id)?;
        self.publish_terminal(
            api,
            step,
            active.operation_id,
            active.requester,
            active.request_id,
            outcome,
        )
    }

    fn publish_terminal(
        &mut self,
        api: &Api,
        step: StepContext,
        operation_id: api::navigation::NavigationOperationId,
        requester: ProducerId,
        request_id: api::navigation::RequestId,
        outcome: api::navigation::Outcome,
    ) -> Result<()> {
        self.remember_completed(operation_id, requester);
        api.publish_result(step, operation_id, request_id, outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use phoxal::__private::{ClockSource, TestClock, TestHarness, run_test_harness_with_clock};
    use phoxal_bus::{BusConfig, BusOwner, Querier, StatePublisher, StepToken, Subscriber};

    use super::*;

    fn producer(value: u128) -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | value).expect("canonical producer")
    }

    #[test]
    fn server_operation_id_is_scoped_to_navigation_producer() {
        let first = NavigationState::new(producer(1));
        let second = NavigationState::new(producer(2));
        let mut first = first;
        let mut second = second;
        let first_id = first.next_operation_id().unwrap();
        let second_id = second.next_operation_id().unwrap();
        assert_ne!(first_id, second_id);
        assert_eq!(first_id.sequence, second_id.sequence);
        assert_ne!(first_id.producer, second_id.producer);
    }

    #[test]
    fn timeline_reset_does_not_reuse_server_operation_sequence() {
        let mut state = NavigationState::new(producer(3));
        let first = state.next_operation_id().unwrap();
        state.reset();
        let second = state.next_operation_id().unwrap();
        assert_eq!(first.producer, second.producer);
        assert_eq!(second.sequence, first.sequence + 1);
    }

    #[test]
    fn operation_sequence_exhaustion_refuses_without_reusing_the_maximum_id() {
        let mut state = NavigationState::new(producer(13));
        state.next_operation_sequence = u64::MAX;
        assert!(state.next_operation_id().is_none());
        assert_eq!(state.next_operation_sequence, u64::MAX);
    }

    #[test]
    fn input_gates_reject_future_stale_and_replaced_world_samples() {
        let line = TimelineId::mint();
        let other = TimelineId::mint();
        let at = |timeline, ticks| RobotInstant::new(timeline, ticks);
        let state_at = |timeline, ticks| {
            let mut state = NavigationState::new(producer(8));
            state.last_localize = Some(Timed::new(
                api::localize::LocalizationState {
                    x_m: 0.0,
                    y_m: 0.0,
                    yaw_rad: 0.0,
                    confidence: 1.0,
                },
                at(timeline, ticks),
            ));
            state
        };

        assert!(
            state_at(line, 100)
                .fresh_localization(at(line, 100))
                .is_some()
        );
        assert!(
            state_at(line, 101)
                .fresh_localization(at(line, 100))
                .is_none(),
            "a sample from the reference's future is not a fresh observation"
        );
        let stale = u64::try_from(LOCALIZATION_STALE.as_nanos()).unwrap() + 1;
        assert!(
            state_at(line, 0)
                .fresh_localization(at(line, stale))
                .is_none()
        );
        assert!(
            state_at(line, 100)
                .fresh_localization(at(other, 100))
                .is_none(),
            "a sample from a replaced world is incomparable, never fresh"
        );
        assert!(
            NavigationState::new(producer(9))
                .fresh_map_revision(at(line, 100))
                .is_none(),
            "a revision that never arrived is never fresh"
        );
    }

    #[test]
    fn completed_operation_cache_is_bounded_and_forgets_old_ids() {
        let mut state = NavigationState::new(producer(10));
        for sequence in 1..=(RESULT_CACHE_CAPACITY as u64 + 1) {
            state.remember_completed(
                api::navigation::NavigationOperationId {
                    producer: producer(10),
                    sequence,
                },
                producer(11),
            );
        }

        assert_eq!(state.completed.len(), RESULT_CACHE_CAPACITY);
        assert_eq!(
            state.completed_owner(api::navigation::NavigationOperationId {
                producer: producer(10),
                sequence: 1,
            }),
            None
        );
        assert_eq!(
            state.completed_owner(api::navigation::NavigationOperationId {
                producer: producer(10),
                sequence: RESULT_CACHE_CAPACITY as u64 + 1,
            }),
            Some(producer(11))
        );
    }

    #[test]
    fn requester_scope_keeps_same_request_id_independent() {
        let mut state = NavigationState::new(producer(4));
        let request_id = request_id("same");
        state.remember_start(
            producer(1),
            request_id.clone(),
            api::navigation::StartResponse::Refused(api::navigation::RefusalReason::Busy),
        );
        assert!(state.cached_start(producer(1), &request_id).is_some());
        assert!(state.cached_start(producer(2), &request_id).is_none());
    }

    #[test]
    fn foreign_requester_cannot_cancel_an_active_operation() {
        let owner = producer(5);
        let foreign = producer(6);
        let operation_id = api::navigation::NavigationOperationId {
            producer: producer(7),
            sequence: 1,
        };
        let mut state = NavigationState::new(producer(7));
        state.active = Some(Active {
            operation_id,
            requester: owner,
            request_id: request_id("owned"),
            path: api::navigation::Path {
                poses: vec![api::navigation::Pose {
                    x_m: 1.0,
                    y_m: 0.0,
                    yaw_rad: None,
                }],
                map_revision: None,
            },
            accepted_published: true,
            cancel_requested: false,
            started_at: RobotInstant::new(TimelineId::mint(), 0),
        });
        assert_eq!(
            state.cancel(foreign, operation_id),
            api::navigation::CancelResponse::Refused(api::navigation::RefusalReason::NotOwner)
        );
        assert!(!state.active.as_ref().unwrap().cancel_requested);
        assert_eq!(
            state.cancel(owner, operation_id),
            api::navigation::CancelResponse::Accepted
        );
        assert!(state.active.as_ref().unwrap().cancel_requested);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn start_cancel_queries_are_idempotent_and_lossless_over_the_real_bus() {
        let participant = phoxal_bus::ParticipantId::new("navigation-test")
            .expect("valid navigation participant");
        let (owner, bus) = BusOwner::open(BusConfig::in_process(participant))
            .await
            .expect("open shared bus");
        let start = Querier::new(
            bus.clone(),
            &api::topic::client().navigation().start(),
            Duration::from_secs(2),
        )
        .expect("build start querier");
        let cancel = Querier::new(
            bus.clone(),
            &api::topic::client().navigation().cancel(),
            Duration::from_secs(2),
        )
        .expect("build cancel querier");
        let localization = StatePublisher::<api::localize::LocalizationState>::new(
            bus.clone(),
            &api::topic::owner().localize().state(),
        )
        .expect("build localization publisher");
        let map_revision = StatePublisher::<api::map::Revision>::new(
            bus.clone(),
            &api::topic::owner().map().revision(),
        )
        .expect("build map revision publisher");
        let results = Subscriber::<api::navigation::Result>::new(
            &bus,
            &api::topic::client().navigation().result(),
        )
        .await
        .expect("subscribe results");

        let clock = TestClock::new();
        let runner_clock = clock.clone();
        let runner = run_test_harness_with_clock::<Navigation, _, _>(
            &bus,
            TestHarness::new("navigation-test").expect("valid test participant"),
            runner_clock,
            async {
                tokio::time::sleep(Duration::from_secs(3)).await;
            },
        );
        let client = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let step = StepToken::mint(clock.read().instant().expect("test clock is synchronized"));
            localization
                .publish(
                    &step,
                    api::localize::LocalizationState {
                        x_m: 0.0,
                        y_m: 0.0,
                        yaw_rad: 0.0,
                        confidence: 1.0,
                    },
                )
                .expect("publish localization");
            map_revision
                .publish(
                    &step,
                    api::map::Revision {
                        revision: 7,
                        resolution_m: 0.05,
                    },
                )
                .expect("publish map revision");
            tokio::time::sleep(Duration::from_millis(100)).await;

            let same_request_id = request_id("same-request");
            let first = start
                .query(start_request(same_request_id.clone(), 0.0))
                .await
                .expect("first start query");
            let operation_id = match first {
                api::navigation::StartResponse::Accepted { operation_id } => operation_id,
                other => panic!("first start was not admitted: {other:?}"),
            };
            let replay = start
                .query(start_request(same_request_id, 99.0))
                .await
                .expect("same requester retry");
            assert_eq!(
                replay,
                api::navigation::StartResponse::Accepted { operation_id },
                "requester-scoped idempotency returns the original operation"
            );
            let completed = await_result(&results, operation_id).await;
            assert!(matches!(
                completed.outcome,
                api::navigation::Outcome::Succeeded
            ));

            let moving = start
                .query(start_request(request_id("moving"), 5.0))
                .await
                .expect("moving start query");
            let moving_id = match moving {
                api::navigation::StartResponse::Accepted { operation_id } => operation_id,
                other => panic!("moving start was not admitted: {other:?}"),
            };
            assert_eq!(
                cancel
                    .query(api::navigation::CancelRequest {
                        operation_id: moving_id,
                    })
                    .await
                    .expect("cancel query"),
                api::navigation::CancelResponse::Accepted
            );
            let cancelled = await_result(&results, moving_id).await;
            assert!(matches!(
                cancelled.outcome,
                api::navigation::Outcome::Cancelled
            ));

            // Query receive tasks apply back-pressure to this bounded queue;
            // every request still gets a typed response under pressure.
            let mut requests = Vec::new();
            for index in 0..96_u32 {
                let querier = start.clone();
                requests.push(tokio::spawn(async move {
                    querier
                        .query(start_request(
                            api::navigation::RequestId {
                                value: format!("pressure-{index}"),
                            },
                            0.0,
                        ))
                        .await
                }));
            }
            for request in requests {
                let response = request.await.expect("pressure query task").expect("reply");
                assert!(matches!(
                    response,
                    api::navigation::StartResponse::Accepted { .. }
                        | api::navigation::StartResponse::Refused(_)
                ));
            }
        };

        let (runner_result, ()) = tokio::join!(runner, client);
        runner_result.expect("navigation runner completed cleanly");
        owner.close().await.expect("close shared bus");
    }

    fn request_id(value: &str) -> api::navigation::RequestId {
        api::navigation::RequestId {
            value: value.to_string(),
        }
    }

    fn start_request(
        request_id: api::navigation::RequestId,
        x_m: f64,
    ) -> api::navigation::StartRequest {
        api::navigation::StartRequest {
            request_id,
            kind: api::navigation::StartKind::GotoPose(api::navigation::Pose {
                x_m,
                y_m: 0.0,
                yaw_rad: Some(0.0),
            }),
        }
    }

    async fn await_result(
        results: &Subscriber<api::navigation::Result>,
        operation_id: api::navigation::NavigationOperationId,
    ) -> api::navigation::Result {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let received = results.recv().await.expect("receive navigation result");
                if received.body.operation_id == operation_id {
                    return received.body;
                }
            }
        })
        .await
        .expect("navigation result arrived before deadline")
    }
}
