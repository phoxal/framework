//! `navigation` owns request lifecycle, planning, path following, frontier
//! proposal, cancellation, progress, and terminal results.

use anyhow::Result;
use std::time::Duration;

use phoxal::api;
use phoxal::bus::QueryFailure;
use phoxal::prelude::*;
use std::collections::{BTreeMap, VecDeque};

use crate::follower;
use crate::frontiers::OccupancyGrid;
use crate::planner;

/// How long an input sample stays usable. Both the pose and the map revision
/// are inputs to the same steering decision, so they share one horizon: a fresh
/// pose against a stale map is not a safer basis for driving than a stale pose.
const LOCALIZATION_STALE: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const RESULT_CACHE_CAPACITY: usize = 1024;

struct Active {
    request_id: api::navigation::RequestId,
    path: api::navigation::Path,
    accepted_published: bool,
    started_at: RobotInstant,
}

pub(crate) struct Api {
    request: Subscriber<api::navigation::Request>,
    localize: Subscriber<api::localize::LocalizationState>,
    map_revision: Subscriber<api::map::Revision>,
    map_submap: Querier<api::map::SubmapRequest, api::map::SubmapResponse>,
    state: StatePublisher<api::navigation::State>,
    progress: StatePublisher<api::navigation::Progress>,
    result: StatePublisher<api::navigation::Result>,
    candidate: StatePublisher<api::navigation::Candidate>,
}

#[derive(Default)]
pub(crate) struct NavigationState {
    active: Option<Active>,
    last_localize: Option<Timed<api::localize::LocalizationState>>,
    last_map_revision: Option<Timed<api::map::Revision>>,
    completed: BTreeMap<api::navigation::RequestId, api::navigation::Outcome>,
    completion_order: VecDeque<api::navigation::RequestId>,
    last_time: Option<RobotInstant>,
}

#[phoxal::service(state = NavigationState, api = Api)]
pub(crate) struct Navigation;

impl Participant for Navigation {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        ctx.query(
            api::topic::owner().navigation().next_frontier(),
            Self::next_frontier,
        )
        .await?;
        Ok((
            NavigationState::default(),
            Api {
                request: ctx
                    .subscriber(api::topic::owner().navigation().request())
                    .await?,
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

        while let Some(received) = api.request.try_recv() {
            state.handle_request(api, step, received.body)?;
        }

        // Both gates are pure functions of what arrived above, so reading them
        // once here keeps the rest of the step working from a single consistent
        // view of the inputs.
        let localization = state.fresh_localization(now);
        let map_revision = state.fresh_map_revision(now);

        let Some(active) = state.active.as_mut() else {
            api.state
                .publish(&step.token, api::navigation::State::Idle)?;
            return Ok(());
        };

        if !active.accepted_published {
            active.accepted_published = true;
            api.state.publish(
                &step.token,
                api::navigation::State::Accepted(active.request_id.clone()),
            )?;
            return Ok(());
        }

        // A request that started on a replaced timeline is not aged here: there
        // is no ordering across timelines to age it by. It still terminates,
        // because the localization gate below fails closed on exactly that
        // mismatch.
        let timed_out = now
            .duration_since(active.started_at)
            .is_ok_and(|elapsed| elapsed > REQUEST_TIMEOUT);
        if timed_out {
            return state.abandon_active(api, step, api::navigation::Outcome::TimedOut);
        }

        if active.path.map_revision.is_some_and(|expected| {
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

        let Some(localization) = localization else {
            return state.abandon_active(
                api,
                step,
                api::navigation::Outcome::Failed(
                    api::navigation::FailureReason::LocalizationUnavailable,
                ),
            );
        };

        let Some(output) = follower::pursue(&active.path, &localization.body) else {
            return state.abandon_active(
                api,
                step,
                api::navigation::Outcome::Failed(api::navigation::FailureReason::NoPath),
            );
        };

        let request_id = active.request_id.clone();
        api.state.publish(
            &step.token,
            api::navigation::State::Running(request_id.clone()),
        )?;
        api.progress.publish(
            &step.token,
            api::navigation::Progress {
                request_id: request_id.clone(),
                distance_remaining_m: output.distance_remaining_m,
                path_index: output.target_index as u32,
            },
        )?;
        api.candidate.publish(
            &step.token,
            api::navigation::Candidate {
                request_id: request_id.clone(),
                linear_x_mps: output.linear_x_mps,
                angular_z_radps: output.angular_z_radps,
            },
        )?;

        if output.finished {
            state.active = None;
            state.publish_terminal(api, step, request_id, api::navigation::Outcome::Succeeded)?;
        }
        Ok(())
    }

    fn reset(&self, _ctx: ResetContext, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        *state = NavigationState::default();
        Ok(())
    }
}

impl Navigation {
    async fn next_frontier(
        &self,
        api: &Api,
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
        let submap_request = api::map::SubmapRequest {
            min_x_m: 0.0,
            min_y_m: 0.0,
            max_x_m: extent,
            max_y_m: extent,
        };
        let response = api
            .map_submap
            .query(submap_request)
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
        request_id: api::navigation::RequestId,
        outcome: api::navigation::Outcome,
    ) -> Result<()> {
        self.result.publish(
            &step.token,
            api::navigation::Result {
                request_id,
                outcome,
            },
        )?;
        Ok(())
    }

    /// Withdraw this request's motion candidate by commanding zero velocity.
    fn publish_zero_candidate(
        &self,
        step: StepContext,
        request_id: api::navigation::RequestId,
    ) -> Result<()> {
        self.candidate.publish(
            &step.token,
            api::navigation::Candidate {
                request_id,
                linear_x_mps: 0.0,
                angular_z_radps: 0.0,
            },
        )?;
        Ok(())
    }
}

impl NavigationState {
    /// The newest pose estimate, if it is recent enough at `now` to act on.
    fn fresh_localization(
        &self,
        now: RobotInstant,
    ) -> Option<Timed<api::localize::LocalizationState>> {
        self.last_localize
            .as_ref()
            .filter(|sample| sample.fresh_within(now, LOCALIZATION_STALE))
            .cloned()
    }

    /// The newest map revision, if it is recent enough at `now` to act on.
    fn fresh_map_revision(&self, now: RobotInstant) -> Option<Timed<api::map::Revision>> {
        self.last_map_revision
            .as_ref()
            .filter(|sample| sample.fresh_within(now, LOCALIZATION_STALE))
            .cloned()
    }

    /// End the active request with `outcome`.
    ///
    /// The motion candidate is withdrawn before the result is published, so no
    /// consumer can see a request reach a terminal state while the last velocity
    /// it commanded still stands. Does nothing when no request is active, which
    /// is the right no-op for the callers that have already established one is
    /// there.
    fn abandon_active(
        &mut self,
        api: &Api,
        step: StepContext,
        outcome: api::navigation::Outcome,
    ) -> Result<()> {
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        api.publish_zero_candidate(step, active.request_id.clone())?;
        self.publish_terminal(api, step, active.request_id, outcome)
    }

    fn publish_terminal(
        &mut self,
        api: &Api,
        step: StepContext,
        request_id: api::navigation::RequestId,
        outcome: api::navigation::Outcome,
    ) -> Result<()> {
        self.remember_terminal(&request_id, &outcome);
        api.publish_result(step, request_id, outcome)
    }

    fn remember_terminal(
        &mut self,
        request_id: &api::navigation::RequestId,
        outcome: &api::navigation::Outcome,
    ) {
        if !self.completed.contains_key(request_id) {
            if self.completion_order.len() == RESULT_CACHE_CAPACITY
                && let Some(oldest) = self.completion_order.pop_front()
            {
                self.completed.remove(&oldest);
            }
            self.completion_order.push_back(request_id.clone());
        }
        self.completed.insert(request_id.clone(), outcome.clone());
    }

    fn handle_request(
        &mut self,
        api: &Api,
        step: StepContext,
        request: api::navigation::Request,
    ) -> Result<()> {
        if !request.request_id.is_valid() {
            return api.publish_result(
                step,
                request.request_id,
                api::navigation::Outcome::Refused(api::navigation::RefusalReason::InvalidRequest),
            );
        }
        if let Some(active) = &self.active
            && active.request_id == request.request_id
        {
            let state = if active.accepted_published {
                api::navigation::State::Running(active.request_id.clone())
            } else {
                api::navigation::State::Accepted(active.request_id.clone())
            };
            api.state.publish(&step.token, state)?;
            return Ok(());
        }
        if let Some(outcome) = self.completed.get(&request.request_id).cloned() {
            return api.publish_result(step, request.request_id, outcome);
        }
        match request.kind {
            api::navigation::RequestKind::Cancel(target_request_id) => {
                let cancels_active = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.request_id == target_request_id);
                if cancels_active {
                    self.abandon_active(api, step, api::navigation::Outcome::Cancelled)?;
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Succeeded,
                    )?;
                } else {
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Refused(
                            api::navigation::RefusalReason::InvalidRequest,
                        ),
                    )?;
                }
            }
            api::navigation::RequestKind::GotoPose(goal) => {
                if self.active.is_some() {
                    return self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Refused(api::navigation::RefusalReason::Busy),
                    );
                }
                let Some(localization) = self.fresh_localization(step.now()) else {
                    return self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Failed(
                            api::navigation::FailureReason::LocalizationUnavailable,
                        ),
                    );
                };
                let Some(revision) = self.fresh_map_revision(step.now()) else {
                    return self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Failed(
                            api::navigation::FailureReason::MapUnavailable,
                        ),
                    );
                };
                let Some(path) =
                    planner::straight_line(&localization.body, &goal, Some(revision.body.revision))
                else {
                    return self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Refused(
                            api::navigation::RefusalReason::InvalidRequest,
                        ),
                    );
                };
                self.active = Some(Active {
                    request_id: request.request_id,
                    path,
                    accepted_published: false,
                    started_at: step.now(),
                });
            }
            api::navigation::RequestKind::FollowPath(path) => {
                if self.active.is_some() {
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Refused(api::navigation::RefusalReason::Busy),
                    )?;
                } else if planner::valid_path(&path)
                    && path.map_revision.is_some_and(|expected| {
                        self.fresh_map_revision(step.now())
                            .is_some_and(|current| current.body.revision == expected)
                    })
                {
                    self.active = Some(Active {
                        request_id: request.request_id,
                        path,
                        accepted_published: false,
                        started_at: step.now(),
                    });
                } else {
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Refused(
                            api::navigation::RefusalReason::InvalidRequest,
                        ),
                    )?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use phoxal::__private::{ClockSource, TestClock, TestHarness, run_test_harness_with_clock};
    use phoxal_bus::{
        BusConfig, BusOwner, CommandPublisher, StatePublisher, StepToken, Subscriber,
    };

    use super::*;

    #[test]
    fn input_gates_reject_future_stale_and_replaced_world_samples() {
        let line = TimelineId::mint();
        let other = TimelineId::mint();
        let at = |timeline, ticks| RobotInstant::new(timeline, ticks);
        let state_at = |timeline, ticks| NavigationState {
            last_localize: Some(Timed::new(
                api::localize::LocalizationState {
                    x_m: 0.0,
                    y_m: 0.0,
                    yaw_rad: 0.0,
                    confidence: 1.0,
                },
                at(timeline, ticks),
            )),
            ..NavigationState::default()
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
            NavigationState::default()
                .fresh_map_revision(at(line, 100))
                .is_none(),
            "a revision that never arrived is never fresh"
        );
    }

    #[test]
    fn terminal_results_are_replayable_and_bounded() {
        let mut navigation = NavigationState::default();
        for index in 0..=RESULT_CACHE_CAPACITY {
            navigation.remember_terminal(
                &request_id(&format!("request-{index}")),
                &api::navigation::Outcome::Succeeded,
            );
        }

        assert_eq!(navigation.completed.len(), RESULT_CACHE_CAPACITY);
        assert_eq!(navigation.completion_order.len(), RESULT_CACHE_CAPACITY);
        assert!(!navigation.completed.contains_key(&request_id("request-0")));
        assert_eq!(
            navigation.completed.get(&request_id("request-1024")),
            Some(&api::navigation::Outcome::Succeeded)
        );

        navigation.remember_terminal(
            &request_id("request-1024"),
            &api::navigation::Outcome::Cancelled,
        );
        assert_eq!(navigation.completed.len(), RESULT_CACHE_CAPACITY);
        assert_eq!(
            navigation.completed.get(&request_id("request-1024")),
            Some(&api::navigation::Outcome::Cancelled)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lifecycle_runs_replays_and_cancels_over_the_real_bus() {
        let (owner, bus) = BusOwner::open(BusConfig::in_process(
            phoxal_bus::ParticipantId::new("navigation-lifecycle").expect("valid participant id"),
        ))
        .await
        .expect("open shared bus");
        let request = CommandPublisher::<api::navigation::Request>::new(
            bus.clone(),
            &api::topic::client().navigation().request(),
        )
        .expect("build request publisher");
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
        let states = Subscriber::<api::navigation::State>::new(
            &bus,
            &api::topic::client().navigation().state(),
        )
        .await
        .expect("subscribe state");
        let results = Subscriber::<api::navigation::Result>::new(
            &bus,
            &api::topic::client().navigation().result(),
        )
        .await
        .expect("subscribe result");
        let candidates = Subscriber::<api::navigation::Candidate>::new(
            &bus,
            &api::topic::client().navigation().candidate(),
        )
        .await
        .expect("subscribe candidate");

        let clock = TestClock::new();
        let runner_clock = clock.clone();
        let runner = run_test_harness_with_clock::<Navigation, _, _>(
            &bus,
            TestHarness::new("navigation-1").expect("valid test participant"),
            runner_clock,
            async { tokio::time::sleep(Duration::from_millis(900)).await },
        );
        let client = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            // Stand in for the upstream services: the test drives the same
            // clock the runner reads, so its stamps land on the runner's
            // own timeline.
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

            let immediate_id = request_id("immediate");
            request
                .send(api::navigation::Request {
                    request_id: immediate_id.clone(),
                    kind: api::navigation::RequestKind::GotoPose(api::navigation::Pose {
                        x_m: 0.0,
                        y_m: 0.0,
                        yaw_rad: Some(0.0),
                    }),
                })
                .expect("publish immediate request");
            await_state(&states, |state| {
                matches!(state, api::navigation::State::Accepted(id) if id == &immediate_id)
            })
            .await;
            await_state(
                &states,
                |state| matches!(state, api::navigation::State::Running(id) if id == &immediate_id),
            )
            .await;
            let succeeded = await_result(&results, "immediate").await;
            assert!(matches!(
                succeeded.outcome,
                api::navigation::Outcome::Succeeded
            ));

            request
                .send(api::navigation::Request {
                    request_id: immediate_id,
                    kind: api::navigation::RequestKind::GotoPose(api::navigation::Pose {
                        x_m: 99.0,
                        y_m: 99.0,
                        yaw_rad: None,
                    }),
                })
                .expect("replay completed request");
            let replayed = await_result(&results, "immediate").await;
            assert!(matches!(
                replayed.outcome,
                api::navigation::Outcome::Succeeded
            ));

            let moving_id = request_id("moving");
            request
                .send(api::navigation::Request {
                    request_id: moving_id.clone(),
                    kind: api::navigation::RequestKind::GotoPose(api::navigation::Pose {
                        x_m: 5.0,
                        y_m: 0.0,
                        yaw_rad: None,
                    }),
                })
                .expect("publish moving request");
            await_state(
                &states,
                |state| matches!(state, api::navigation::State::Accepted(id) if id == &moving_id),
            )
            .await;

            request
                .send(api::navigation::Request {
                    request_id: request_id("cancel-moving"),
                    kind: api::navigation::RequestKind::Cancel(moving_id),
                })
                .expect("publish cancellation");
            let cancelled = await_result(&results, "moving").await;
            assert!(matches!(
                cancelled.outcome,
                api::navigation::Outcome::Cancelled
            ));
            let cancel_result = await_result(&results, "cancel-moving").await;
            assert!(matches!(
                cancel_result.outcome,
                api::navigation::Outcome::Succeeded
            ));
            let zero = await_candidate(&candidates, "moving").await;
            assert_eq!(zero.linear_x_mps, 0.0);
            assert_eq!(zero.angular_z_radps, 0.0);
        };

        let (runner_result, ()) = tokio::join!(runner, client);
        runner_result.expect("navigation runner completed cleanly");
        owner.close().await.expect("close shared bus");
    }

    async fn await_state(
        states: &Subscriber<api::navigation::State>,
        matches: impl Fn(&api::navigation::State) -> bool,
    ) {
        tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                let received = states.recv().await.expect("receive navigation state");
                if matches(&received.body) {
                    return;
                }
            }
        })
        .await
        .expect("navigation state arrived before deadline");
    }

    async fn await_result(
        results: &Subscriber<api::navigation::Result>,
        request_id: &str,
    ) -> api::navigation::Result {
        tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                let received = results.recv().await.expect("receive navigation result");
                if received.body.request_id.value == request_id {
                    return received.body;
                }
            }
        })
        .await
        .expect("navigation result arrived before deadline")
    }

    async fn await_candidate(
        candidates: &Subscriber<api::navigation::Candidate>,
        request_id: &str,
    ) -> api::navigation::Candidate {
        tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                let received = candidates
                    .recv()
                    .await
                    .expect("receive navigation candidate");
                if received.body.request_id.value == request_id {
                    return received.body;
                }
            }
        })
        .await
        .expect("navigation candidate arrived before deadline")
    }

    fn request_id(value: &str) -> api::navigation::RequestId {
        api::navigation::RequestId {
            value: value.to_string(),
        }
    }
}
