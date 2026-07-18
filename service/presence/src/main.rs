//! `presence` - aggregate explicit participant heartbeats.
//!
//! Per plan #00 the heartbeat seam is split by role. Participants publish their
//! own beacon on the `command`-role `presence/heartbeat` topic; this participant
//! subscribes them (its control input) and publishes one aggregate readiness on
//! the `state`-role `presence/state` topic, which clients subscribe.
//!
//! It tracks the last-seen time and readiness of every participant and folds
//! them into one readiness value. Aggregation is worst-wins and fail-aware: any
//! `Failed` participant makes the aggregate `Failed`; a participant whose last
//! heartbeat is older than the stale threshold (3 s) counts as `Degraded`;
//! otherwise `Degraded` beats `Initializing`/`NotStarted`, which beats `Ready`.
//! The participant's own id is excluded from the fold so it cannot poison its own
//! aggregate. Having observed nobody at all is never `Ready` either - an empty
//! tracker has no basis for that claim, so it folds to `Initializing` instead of
//! defaulting to the vacuously-true "no failures seen" reading.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::v1 as api;

const PARTICIPANT: &str = "presence";
const STALE_AFTER: Duration = Duration::from_secs(3);
const RETAIN_AFTER: Duration = Duration::from_secs(30);
const MAX_TRACKED_PARTICIPANTS: usize = 1_024;
const MAX_PARTICIPANT_ID_BYTES: usize = 256;

#[derive(phoxal::Api)]
struct Api {
    heartbeats: Subscriber<api::presence::Heartbeat>,
    state_pub: Publisher<api::presence::State>,
}

#[phoxal::service(id = "presence", config = ())]
struct Presence {
    tracker: ReadinessTracker,
}

#[phoxal::behavior]
impl Presence {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        Ok((
            Self {
                tracker: ReadinessTracker::default(),
            },
            Self::Api {
                heartbeats: ctx
                    .subscriber(api::topic::internal::new(cap).presence().heartbeat(), 32)
                    .await?,
                state_pub: ctx
                    .publisher(api::topic::internal::new(cap).presence().state())
                    .await?,
            },
        ))
    }

    #[step(hz = 1)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        while let Some(received) = api.heartbeats.try_recv() {
            self.tracker.ingest(received.body, Instant::now());
        }

        let now = Instant::now();
        self.tracker.prune(now);
        let readiness = self.tracker.aggregate(now);
        api.state_pub
            .publish_at(step.time(), api::presence::State { readiness })
            .await?;
        Ok(())
    }
}

#[derive(Debug, Default)]
struct ReadinessTracker {
    participants: BTreeMap<String, ParticipantRecord>,
}

impl ReadinessTracker {
    fn ingest(&mut self, heartbeat: api::presence::Heartbeat, received_at: Instant) {
        if heartbeat.participant.is_empty()
            || heartbeat.participant.len() > MAX_PARTICIPANT_ID_BYTES
        {
            return;
        }
        if !self.participants.contains_key(&heartbeat.participant)
            && self.participants.len() >= MAX_TRACKED_PARTICIPANTS
            && let Some(oldest) = self
                .participants
                .iter()
                .min_by_key(|(_, record)| record.received_at)
                .map(|(participant, _)| participant.clone())
        {
            self.participants.remove(&oldest);
        }
        self.participants.insert(
            heartbeat.participant,
            ParticipantRecord {
                readiness: heartbeat.readiness,
                received_at,
            },
        );
    }

    fn prune(&mut self, now: Instant) {
        self.participants
            .retain(|_, record| now.saturating_duration_since(record.received_at) <= RETAIN_AFTER);
    }

    fn aggregate(&self, now: Instant) -> api::presence::Readiness {
        let mut saw_initializing = false;
        let mut saw_degraded = false;
        let mut saw_any = false;

        for (participant, record) in &self.participants {
            if participant == PARTICIPANT {
                continue;
            }
            saw_any = true;
            if is_stale(now, record.received_at) {
                saw_degraded = true;
                continue;
            }

            match record.readiness {
                api::presence::Readiness::Failed => return api::presence::Readiness::Failed,
                api::presence::Readiness::Degraded => saw_degraded = true,
                api::presence::Readiness::Initializing | api::presence::Readiness::NotStarted => {
                    saw_initializing = true
                }
                api::presence::Readiness::Ready => {}
            }
        }

        if saw_degraded {
            api::presence::Readiness::Degraded
        } else if saw_initializing || !saw_any {
            // `!saw_any`: nobody has ever been observed (including at boot,
            // before the first heartbeat arrives). Claiming `Ready` here would
            // be a vacuous truth - "no failures" is not the same as "checked
            // and healthy" - and is exactly the bug that let a graph with every
            // participant silently missing still read as ready.
            api::presence::Readiness::Initializing
        } else {
            api::presence::Readiness::Ready
        }
    }
}

#[derive(Debug)]
struct ParticipantRecord {
    readiness: api::presence::Readiness,
    received_at: Instant,
}

fn is_stale(now: Instant, received_at: Instant) -> bool {
    now.saturating_duration_since(received_at) > STALE_AFTER
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Presence>()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use phoxal::participant::{ContractRole, Participant, ParticipantApi};
    use phoxal_api::ContractBody;
    use phoxal_api::v1 as api;

    use super::{
        MAX_PARTICIPANT_ID_BYTES, MAX_TRACKED_PARTICIPANTS, Presence, RETAIN_AFTER,
        ReadinessTracker, STALE_AFTER, is_stale,
    };

    #[test]
    fn stale_participant_degrades_aggregate() {
        let mut tracker = ReadinessTracker::default();
        let now = Instant::now();
        tracker.ingest(heartbeat("drive", api::presence::Readiness::Ready), now);

        assert_eq!(
            tracker.aggregate(now + STALE_AFTER + Duration::from_nanos(1)),
            api::presence::Readiness::Degraded
        );
    }

    #[test]
    fn failed_participant_fails_aggregate() {
        let mut tracker = ReadinessTracker::default();
        let now = Instant::now();
        tracker.ingest(heartbeat("plan", api::presence::Readiness::Failed), now);

        assert_eq!(tracker.aggregate(now), api::presence::Readiness::Failed);
    }

    #[test]
    fn initializing_participant_keeps_aggregate_initializing() {
        let mut tracker = ReadinessTracker::default();
        let now = Instant::now();
        tracker.ingest(
            heartbeat("localize", api::presence::Readiness::Initializing),
            now,
        );

        assert_eq!(
            tracker.aggregate(now),
            api::presence::Readiness::Initializing
        );
    }

    #[test]
    fn ready_participants_are_ready() {
        let mut tracker = ReadinessTracker::default();
        let now = Instant::now();
        tracker.ingest(heartbeat("drive", api::presence::Readiness::Ready), now);
        tracker.ingest(heartbeat("safety", api::presence::Readiness::Ready), now);

        assert_eq!(tracker.aggregate(now), api::presence::Readiness::Ready);
    }

    #[test]
    fn own_heartbeat_does_not_poison_aggregate() {
        let mut tracker = ReadinessTracker::default();
        let now = Instant::now();
        tracker.ingest(
            heartbeat("presence", api::presence::Readiness::Degraded),
            now,
        );
        tracker.ingest(heartbeat("drive", api::presence::Readiness::Ready), now);

        assert_eq!(tracker.aggregate(now), api::presence::Readiness::Ready);
    }

    #[test]
    fn nobody_observed_is_never_ready() {
        let tracker = ReadinessTracker::default();

        assert_eq!(
            tracker.aggregate(Instant::now()),
            api::presence::Readiness::Initializing
        );
    }

    #[test]
    fn only_own_heartbeat_observed_is_never_ready() {
        let mut tracker = ReadinessTracker::default();
        let now = Instant::now();
        tracker.ingest(heartbeat("presence", api::presence::Readiness::Ready), now);

        assert_eq!(
            tracker.aggregate(now),
            api::presence::Readiness::Initializing
        );
    }

    #[test]
    fn stale_threshold_is_exclusive() {
        let now = Instant::now();
        assert!(!is_stale(now + STALE_AFTER, now));
        assert!(is_stale(now + STALE_AFTER + Duration::from_nanos(1), now));
    }

    #[test]
    fn departed_participant_is_eventually_evicted_and_ready_can_recover() {
        let mut tracker = ReadinessTracker::default();
        let now = Instant::now();
        tracker.ingest(heartbeat("departed", api::presence::Readiness::Ready), now);
        tracker.ingest(
            heartbeat("survivor", api::presence::Readiness::Ready),
            now + RETAIN_AFTER,
        );

        tracker.prune(now + RETAIN_AFTER + Duration::from_nanos(1));

        assert_eq!(tracker.participants.len(), 1);
        assert_eq!(
            tracker.aggregate(now + RETAIN_AFTER + Duration::from_nanos(1)),
            api::presence::Readiness::Ready
        );
    }

    #[test]
    fn tracker_rejects_oversized_ids_and_evicts_at_its_cardinality_cap() {
        let mut tracker = ReadinessTracker::default();
        let now = Instant::now();
        tracker.ingest(
            heartbeat(
                &"x".repeat(MAX_PARTICIPANT_ID_BYTES + 1),
                api::presence::Readiness::Ready,
            ),
            now,
        );
        assert!(tracker.participants.is_empty());

        for index in 0..=MAX_TRACKED_PARTICIPANTS {
            let elapsed_nanos = u64::try_from(index).expect("test index fits in u64");
            tracker.ingest(
                heartbeat(
                    &format!("participant-{index}"),
                    api::presence::Readiness::Ready,
                ),
                now + Duration::from_nanos(elapsed_nanos),
            );
        }
        assert_eq!(tracker.participants.len(), MAX_TRACKED_PARTICIPANTS);
        assert!(!tracker.participants.contains_key("participant-0"));
        assert!(
            tracker
                .participants
                .contains_key(&format!("participant-{MAX_TRACKED_PARTICIPANTS}"))
        );
    }

    #[test]
    fn api_reports_contracts() {
        assert_eq!(<Presence as Participant>::ID, "presence");

        let contracts = <<Presence as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert!(contracts.iter().any(|c| {
            c.topic == <api::presence::Heartbeat as ContractBody>::TOPIC
                && c.role == ContractRole::Subscribe
        }));
        assert!(contracts.iter().any(|c| {
            c.topic == <api::presence::State as ContractBody>::TOPIC
                && c.role == ContractRole::Publish
        }));
    }

    fn heartbeat(
        participant: &str,
        readiness: api::presence::Readiness,
    ) -> api::presence::Heartbeat {
        api::presence::Heartbeat {
            participant: participant.to_string(),
            readiness,
        }
    }
}
