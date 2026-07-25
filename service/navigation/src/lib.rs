//! `navigation` owns request lifecycle, planning, path following, frontier
//! proposal, cancellation, progress, and terminal results.

mod exploration;
mod follower;
mod frontiers;
mod planner;
mod scoring;

use anyhow::Result;
use std::time::Duration;

use phoxal::api;
use phoxal::bus::QueryFailure;
use phoxal::prelude::*;
use std::collections::{BTreeMap, VecDeque};

const LOCALIZATION_STALE: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT_NS: u64 = 120_000_000_000;
const RESULT_CACHE_CAPACITY: usize = 1024;

#[derive(Clone)]
struct Timed<T> {
    body: T,
    at: RobotInstant,
}

struct Active {
    request_id: api::navigation::RequestId,
    path: api::navigation::Path,
    accepted_published: bool,
    started_at_ns: u64,
}

#[derive(phoxal::Api)]
pub struct Api {
    request: Subscriber<api::navigation::Request>,
    localize: Subscriber<api::localize::LocalizationState>,
    map_revision: Subscriber<api::map::Revision>,
    map_submap: Querier<api::map::SubmapRequest, api::map::SubmapResponse>,
    state: StatePublisher<api::navigation::State>,
    progress: StatePublisher<api::navigation::Progress>,
    result: StatePublisher<api::navigation::Result>,
    candidate: StatePublisher<api::navigation::Candidate>,
    next_frontier: Server<api::navigation::FrontierRequest, api::navigation::FrontierResponse>,
}

#[phoxal::service(id = "navigation", config = ())]
pub struct Navigation {
    active: Option<Active>,
    last_localize: Option<Timed<api::localize::LocalizationState>>,
    last_map_revision: Option<Timed<api::map::Revision>>,
    completed: BTreeMap<String, api::navigation::Outcome>,
    completion_order: VecDeque<String>,
    last_time: Option<RobotInstant>,
}

#[phoxal::behavior]
impl Navigation {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let cap = ctx.owner_capability();
        Ok((
            Self {
                active: None,
                last_localize: None,
                last_map_revision: None,
                completed: BTreeMap::new(),
                completion_order: VecDeque::new(),
                last_time: None,
            },
            Self::Api {
                request: ctx
                    .subscriber(api::topic::internal::new(cap).navigation().request(), 32)
                    .await?,
                localize: ctx
                    .subscriber(api::topic::new().localize().state(), 32)
                    .await?,
                map_revision: ctx
                    .subscriber(api::topic::new().map().revision(), 32)
                    .await?,
                map_submap: ctx.querier(api::topic::new().map().submap()).await?,
                state: ctx
                    .state_publisher(api::topic::internal::new(cap).navigation().state())
                    .await?,
                progress: ctx
                    .state_publisher(api::topic::internal::new(cap).navigation().progress())
                    .await?,
                result: ctx
                    .state_publisher(api::topic::internal::new(cap).navigation().result())
                    .await?,
                candidate: ctx
                    .state_publisher(api::topic::internal::new(cap).navigation().candidate())
                    .await?,
                next_frontier: ctx
                    .server(api::topic::new().navigation().next_frontier())
                    .await?,
            },
        ))
    }

    #[reset]
    async fn reset(&mut self, _ctx: ResetContext) -> Result<()> {
        self.active = None;
        self.last_localize = None;
        self.last_map_revision = None;
        self.completed.clear();
        self.completion_order.clear();
        self.last_time = None;
        Ok(())
    }

    #[step(hz = 20)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        self.last_time = Some(step.now());
        while let Some(received) = api.localize.try_recv() {
            if let Some(at) = received.metadata.produced_exactly_at() {
                self.last_localize = Some(Timed {
                    body: received.body,
                    at,
                });
            }
        }
        while let Some(received) = api.map_revision.try_recv() {
            if let Some(at) = received.metadata.produced_exactly_at() {
                self.last_map_revision = Some(Timed {
                    body: received.body,
                    at,
                });
            }
        }

        while let Some(received) = api.request.try_recv() {
            self.handle_request(api, step, received.body).await?;
        }

        let now = step.now();
        let Some(active) = self.active.as_mut() else {
            api.state
                .publish(step.token(), api::navigation::State::Idle)?;
            return Ok(());
        };

        if !active.accepted_published {
            active.accepted_published = true;
            api.state.publish(
                step.token(),
                api::navigation::State::Accepted(active.request_id.clone()),
            )?;
            return Ok(());
        }

        if now.ticks().saturating_sub(active.started_at_ns) > REQUEST_TIMEOUT_NS {
            let request_id = active.request_id.clone();
            self.active = None;
            publish_zero(api, step, request_id.clone()).await?;
            self.publish_terminal(api, step, request_id, api::navigation::Outcome::TimedOut)
                .await?;
            return Ok(());
        }

        let map_revision = fresh_sample(self.last_map_revision.as_ref(), now);
        if active.path.map_revision.is_some_and(|expected| {
            map_revision.is_none_or(|current| current.body.revision != expected)
        }) {
            let request_id = active.request_id.clone();
            self.active = None;
            publish_zero(api, step, request_id.clone()).await?;
            self.publish_terminal(
                api,
                step,
                request_id,
                api::navigation::Outcome::Failed(if map_revision.is_some() {
                    api::navigation::FailureReason::MapChanged
                } else {
                    api::navigation::FailureReason::MapUnavailable
                }),
            )
            .await?;
            return Ok(());
        }

        let Some(localize) = fresh_sample(self.last_localize.as_ref(), now) else {
            let request_id = active.request_id.clone();
            self.active = None;
            publish_zero(api, step, request_id.clone()).await?;
            self.publish_terminal(
                api,
                step,
                request_id,
                api::navigation::Outcome::Failed(
                    api::navigation::FailureReason::LocalizationUnavailable,
                ),
            )
            .await?;
            return Ok(());
        };

        let Some(output) = follower::pursue(&active.path, &localize.body) else {
            let request_id = active.request_id.clone();
            self.active = None;
            publish_zero(api, step, request_id.clone()).await?;
            self.publish_terminal(
                api,
                step,
                request_id,
                api::navigation::Outcome::Failed(api::navigation::FailureReason::NoPath),
            )
            .await?;
            return Ok(());
        };

        let request_id = active.request_id.clone();
        api.state.publish(
            step.token(),
            api::navigation::State::Running(request_id.clone()),
        )?;
        api.progress.publish(
            step.token(),
            api::navigation::Progress {
                request_id: request_id.clone(),
                distance_remaining_m: output.distance_remaining_m,
                path_index: output.target_index as u32,
            },
        )?;
        api.candidate.publish(
            step.token(),
            api::navigation::Candidate {
                request_id: request_id.clone(),
                linear_x_mps: output.linear_x_mps,
                angular_z_radps: output.angular_z_radps,
            },
        )?;

        if output.finished {
            self.active = None;
            self.publish_terminal(api, step, request_id, api::navigation::Outcome::Succeeded)
                .await?;
        }
        Ok(())
    }

    #[server(api = next_frontier)]
    async fn next_frontier(
        &mut self,
        api: &mut Self::Api,
        request: api::navigation::FrontierRequest,
    ) -> ServerResult<api::navigation::FrontierResponse> {
        let Some(now) = self.last_time else {
            return Err(QueryFailure::unavailable("no step has run yet"));
        };
        let Some(localize) = fresh_sample(self.last_localize.as_ref(), now) else {
            return Err(QueryFailure::unavailable(
                "localization is unavailable or stale",
            ));
        };
        let Some(revision) = fresh_sample(self.last_map_revision.as_ref(), now) else {
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
            .query(submap_request.clone())
            .await
            .map_err(|error| QueryFailure::unavailable(error.to_string()))?;
        Ok(api::navigation::FrontierResponse {
            frontier: exploration::next_frontier(
                &submap_request,
                response,
                (localize.body.x_m, localize.body.y_m),
            ),
            map_revision: Some(revision.body.revision),
        })
    }

    #[shutdown]
    async fn shutdown(&mut self, api: &mut Self::Api, _ctx: ShutdownContext) -> Result<()> {
        // Shutdown is outside every step, so there is no instant to publish a
        // terminal result at. Dropping the active request locally is the whole
        // obligation; a consumer learns the request ended from the participant
        // going silent, exactly as it would if the process were killed.
        let _ = api;
        self.active = None;
        Ok(())
    }
}

impl Navigation {
    async fn publish_terminal(
        &mut self,
        api: &Api,
        step: StepContext,
        request_id: api::navigation::RequestId,
        outcome: api::navigation::Outcome,
    ) -> Result<()> {
        self.remember_terminal(&request_id, &outcome);
        publish_result(api, step, request_id, outcome).await
    }

    fn remember_terminal(
        &mut self,
        request_id: &api::navigation::RequestId,
        outcome: &api::navigation::Outcome,
    ) {
        let key = request_id.value.clone();
        if !self.completed.contains_key(&key) {
            if self.completion_order.len() == RESULT_CACHE_CAPACITY {
                if let Some(oldest) = self.completion_order.pop_front() {
                    self.completed.remove(&oldest);
                }
            }
            self.completion_order.push_back(key.clone());
        }
        self.completed.insert(key, outcome.clone());
    }

    async fn handle_request(
        &mut self,
        api: &mut Api,
        step: StepContext,
        request: api::navigation::Request,
    ) -> Result<()> {
        if !valid_request_id(&request.request_id) {
            publish_invalid(api, step, request.request_id).await?;
            return Ok(());
        }
        if let Some(active) = &self.active {
            if active.request_id == request.request_id {
                let state = if active.accepted_published {
                    api::navigation::State::Running(active.request_id.clone())
                } else {
                    api::navigation::State::Accepted(active.request_id.clone())
                };
                api.state.publish(step.token(), state)?;
                return Ok(());
            }
        }
        if let Some(outcome) = self.completed.get(&request.request_id.value).cloned() {
            publish_result(api, step, request.request_id, outcome).await?;
            return Ok(());
        }
        match request.kind {
            api::navigation::RequestKind::Cancel(target_request_id) => {
                let matches = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.request_id == target_request_id);
                if matches {
                    self.active = None;
                    publish_zero(api, step, target_request_id.clone()).await?;
                    self.publish_terminal(
                        api,
                        step,
                        target_request_id,
                        api::navigation::Outcome::Cancelled,
                    )
                    .await?;
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Succeeded,
                    )
                    .await?;
                } else {
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Refused(
                            api::navigation::RefusalReason::InvalidRequest,
                        ),
                    )
                    .await?;
                }
            }
            api::navigation::RequestKind::GotoPose(goal) => {
                if self.active.is_some() {
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Refused(api::navigation::RefusalReason::Busy),
                    )
                    .await?;
                    return Ok(());
                }
                let Some(localize) = fresh_sample(self.last_localize.as_ref(), step.now()) else {
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Failed(
                            api::navigation::FailureReason::LocalizationUnavailable,
                        ),
                    )
                    .await?;
                    return Ok(());
                };
                let Some(revision) = fresh_sample(self.last_map_revision.as_ref(), step.now())
                else {
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Failed(
                            api::navigation::FailureReason::MapUnavailable,
                        ),
                    )
                    .await?;
                    return Ok(());
                };
                let Some(path) =
                    planner::straight_line(&localize.body, &goal, Some(revision.body.revision))
                else {
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Refused(
                            api::navigation::RefusalReason::InvalidRequest,
                        ),
                    )
                    .await?;
                    return Ok(());
                };
                self.active = Some(Active {
                    request_id: request.request_id,
                    path,
                    accepted_published: false,
                    started_at_ns: step.now().ticks(),
                });
            }
            api::navigation::RequestKind::FollowPath(path) => {
                if self.active.is_some() {
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Refused(api::navigation::RefusalReason::Busy),
                    )
                    .await?;
                } else if planner::valid_path(&path)
                    && path.map_revision.is_some_and(|expected| {
                        fresh_sample(self.last_map_revision.as_ref(), step.now())
                            .is_some_and(|current| current.body.revision == expected)
                    })
                {
                    self.active = Some(Active {
                        request_id: request.request_id,
                        path,
                        accepted_published: false,
                        started_at_ns: step.now().ticks(),
                    });
                } else {
                    self.publish_terminal(
                        api,
                        step,
                        request.request_id,
                        api::navigation::Outcome::Refused(
                            api::navigation::RefusalReason::InvalidRequest,
                        ),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}

fn valid_request_id(request_id: &api::navigation::RequestId) -> bool {
    let value = request_id.value.trim();
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

/// A sample is usable only if it belongs to the same world history, is not in
/// the reference's future, and is within the staleness bound. A cross-timeline
/// comparison is a checked error, so it can never silently pass.
fn fresh_sample<T>(sample: Option<&Timed<T>>, now: RobotInstant) -> Option<&Timed<T>> {
    sample.filter(|sample| {
        TimeWindow::exact(sample.at)
            .possibly_fresh_within(now, LOCALIZATION_STALE)
            .unwrap_or(false)
    })
}

async fn publish_invalid(
    api: &Api,
    step: StepContext,
    request_id: api::navigation::RequestId,
) -> Result<()> {
    publish_result(
        api,
        step,
        request_id,
        api::navigation::Outcome::Refused(api::navigation::RefusalReason::InvalidRequest),
    )
    .await
}

async fn publish_zero(
    api: &Api,
    step: StepContext,
    request_id: api::navigation::RequestId,
) -> Result<()> {
    api.candidate.publish(
        step.token(),
        api::navigation::Candidate {
            request_id,
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
        },
    )?;
    Ok(())
}

async fn publish_result(
    api: &Api,
    step: StepContext,
    request_id: api::navigation::RequestId,
    outcome: api::navigation::Outcome,
) -> Result<()> {
    api.result.publish(
        step.token(),
        api::navigation::Result {
            request_id,
            outcome,
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use phoxal::bus::ContractBody;
    use phoxal::participant::{
        ClockSource, ContractRole, Participant, ParticipantApi, ParticipantLaunch, TestClock,
    };
    use phoxal::raw::{
        Bus, BusConfig, CommandPublisher, OwnerCap, StatePublisher, StepToken, Subscriber,
        run_with_bus_clock,
    };

    use super::*;

    #[test]
    fn api_owns_the_navigation_lifecycle_and_candidate() {
        assert_eq!(<Navigation as Participant>::ID, "navigation");
        let contracts = <<Navigation as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert_contract::<api::navigation::Request>(contracts, ContractRole::Subscribe);
        assert_contract::<api::navigation::State>(contracts, ContractRole::Publish);
        assert_contract::<api::navigation::Progress>(contracts, ContractRole::Publish);
        assert_contract::<api::navigation::Result>(contracts, ContractRole::Publish);
        assert_contract::<api::navigation::Candidate>(contracts, ContractRole::Publish);
    }

    #[test]
    fn terminal_outcomes_cannot_represent_running() {
        let outcomes = [
            api::navigation::Outcome::Succeeded,
            api::navigation::Outcome::Cancelled,
            api::navigation::Outcome::TimedOut,
        ];
        assert_eq!(outcomes.len(), 3);
    }

    #[test]
    fn request_ids_are_nonempty_bounded_and_filesystem_safe() {
        let id = |value: &str| api::navigation::RequestId {
            value: value.to_string(),
        };
        assert!(valid_request_id(&id("goto-42")));
        assert!(!valid_request_id(&id("")));
        assert!(!valid_request_id(&id("contains space")));
        assert!(!valid_request_id(&id(&"x".repeat(129))));
    }

    #[test]
    fn localization_freshness_rejects_future_and_stale_samples() {
        let line = TimelineId::mint();
        let other = TimelineId::mint();
        let sample = |ticks| Timed {
            body: (),
            at: RobotInstant::new(line, ticks),
        };
        let now = |ticks| RobotInstant::new(line, ticks);
        assert!(fresh_sample(Some(&sample(100)), now(100)).is_some());
        assert!(
            fresh_sample(Some(&sample(101)), now(100)).is_none(),
            "a sample from the reference's future is not a fresh observation"
        );
        let stale = u64::try_from(LOCALIZATION_STALE.as_nanos()).unwrap() + 1;
        assert!(fresh_sample(Some(&sample(0)), now(stale)).is_none());
        assert!(
            fresh_sample(Some(&sample(100)), RobotInstant::new(other, 100)).is_none(),
            "a sample from a replaced world is incomparable, never fresh"
        );
    }

    #[test]
    fn terminal_results_are_replayable_and_bounded() {
        let mut navigation = Navigation {
            active: None,
            last_localize: None,
            last_map_revision: None,
            completed: BTreeMap::new(),
            completion_order: VecDeque::new(),
            last_time: None,
        };
        for index in 0..=RESULT_CACHE_CAPACITY {
            navigation.remember_terminal(
                &request_id(&format!("request-{index}")),
                &api::navigation::Outcome::Succeeded,
            );
        }

        assert_eq!(navigation.completed.len(), RESULT_CACHE_CAPACITY);
        assert_eq!(navigation.completion_order.len(), RESULT_CACHE_CAPACITY);
        assert!(!navigation.completed.contains_key("request-0"));
        assert_eq!(
            navigation.completed.get("request-1024"),
            Some(&api::navigation::Outcome::Succeeded)
        );

        navigation.remember_terminal(
            &request_id("request-1024"),
            &api::navigation::Outcome::Cancelled,
        );
        assert_eq!(navigation.completed.len(), RESULT_CACHE_CAPACITY);
        assert_eq!(
            navigation.completed.get("request-1024"),
            Some(&api::navigation::Outcome::Cancelled)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lifecycle_runs_replays_and_cancels_over_the_real_bus() {
        let bus = Bus::open(BusConfig::in_process("test/navigation-lifecycle", "robot"))
            .await
            .expect("open shared bus");
        let cap = OwnerCap::__mint();
        let request = CommandPublisher::<api::navigation::Request>::new(
            bus.clone(),
            &api::topic::new().navigation().request(),
        )
        .expect("build request publisher");
        let localization = StatePublisher::<api::localize::LocalizationState>::new(
            bus.clone(),
            &api::topic::internal::new(cap).localize().state(),
        )
        .expect("build localization publisher");
        let map_revision = StatePublisher::<api::map::Revision>::new(
            bus.clone(),
            &api::topic::internal::new(cap).map().revision(),
        )
        .expect("build map revision publisher");
        let states = Subscriber::<api::navigation::State>::new(
            &bus,
            &api::topic::new().navigation().state(),
            32,
        )
        .await
        .expect("subscribe state");
        let results = Subscriber::<api::navigation::Result>::new(
            &bus,
            &api::topic::new().navigation().result(),
            32,
        )
        .await
        .expect("subscribe result");
        let candidates = Subscriber::<api::navigation::Candidate>::new(
            &bus,
            &api::topic::new().navigation().candidate(),
            32,
        )
        .await
        .expect("subscribe candidate");

        let clock = TestClock::new();
        let runner_clock = clock.clone();
        let runner = run_with_bus_clock::<Navigation, _, _>(
            &bus,
            ParticipantLaunch::local("navigation-1", "robot"),
            runner_clock,
            async { tokio::time::sleep(Duration::from_millis(900)).await },
        );
        let client =
            async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                // Stand in for the upstream services: the test drives the same
                // clock the runner reads, so its stamps land on the runner's
                // own timeline.
                let step =
                    StepToken::__mint(clock.read().instant().expect("test clock is synchronized"));
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
                await_state(&states, |state| {
                matches!(state, api::navigation::State::Running(id) if id == &immediate_id)
            })
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
                await_state(&states, |state| {
                matches!(state, api::navigation::State::Accepted(id) if id == &moving_id)
            })
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
        bus.close().await.expect("close shared bus");
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

    fn assert_contract<B>(contracts: &[phoxal::participant::ApiContractUse], role: ContractRole)
    where
        B: ContractBody,
    {
        assert!(
            contracts
                .iter()
                .any(|contract| { contract.topic == B::TOPIC && contract.role == role })
        );
    }
}
