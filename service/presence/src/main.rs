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
//! aggregate.

use std::collections::BTreeMap;

use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

const PARTICIPANT: &str = "presence";
const STALE_NS: u64 = 3_000_000_000;

#[derive(phoxal::Service)]
#[phoxal(id = "presence", api = y2026_1)]
struct Presence {
    tracker: ReadinessTracker,
    heartbeats: Subscriber<api::presence::Heartbeat>,
    state_pub: Publisher<api::presence::State>,
}

#[phoxal::behavior]
impl Presence {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        Ok(Self {
            tracker: ReadinessTracker::default(),
            heartbeats: ctx
                .subscribe(api::topic::internal::new(cap).presence().heartbeat())
                .subscriber()
                .await?,
            state_pub: ctx
                .publisher(api::topic::internal::new(cap).presence().state())
                .await?,
        })
    }

    #[step(hz = 1)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        while let Some(received) = self.heartbeats.try_recv() {
            self.tracker
                .ingest(received.body, received.metadata.produced_at_ns);
        }

        let readiness = self.tracker.aggregate(step.time().time_ns());
        self.state_pub
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
    fn ingest(&mut self, heartbeat: api::presence::Heartbeat, last_seen_ns: u64) {
        self.participants.insert(
            heartbeat.participant,
            ParticipantRecord {
                readiness: heartbeat.readiness,
                last_seen_ns,
            },
        );
    }

    fn aggregate(&self, now_ns: u64) -> api::presence::Readiness {
        let mut saw_initializing = false;
        let mut saw_degraded = false;

        for (participant, record) in &self.participants {
            if participant == PARTICIPANT {
                continue;
            }
            if is_stale(now_ns, record.last_seen_ns) {
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
        } else if saw_initializing {
            api::presence::Readiness::Initializing
        } else {
            api::presence::Readiness::Ready
        }
    }
}

#[derive(Debug)]
struct ParticipantRecord {
    readiness: api::presence::Readiness,
    last_seen_ns: u64,
}

fn is_stale(now_ns: u64, last_seen_ns: u64) -> bool {
    now_ns.saturating_sub(last_seen_ns) > STALE_NS
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Presence>()
}

#[cfg(test)]
mod tests {
    use phoxal_api::ContractBody;
    use phoxal_api::y2026_1 as api;

    use super::{Presence, ReadinessTracker, STALE_NS, is_stale};

    #[test]
    fn stale_participant_degrades_aggregate() {
        let mut tracker = ReadinessTracker::default();
        tracker.ingest(heartbeat("drive", api::presence::Readiness::Ready), 1_000);

        assert_eq!(
            tracker.aggregate(1_000 + STALE_NS + 1),
            api::presence::Readiness::Degraded
        );
    }

    #[test]
    fn failed_participant_fails_aggregate() {
        let mut tracker = ReadinessTracker::default();
        tracker.ingest(
            heartbeat("plan", api::presence::Readiness::Failed),
            1_000_000,
        );

        assert_eq!(
            tracker.aggregate(1_000_000),
            api::presence::Readiness::Failed
        );
    }

    #[test]
    fn initializing_participant_keeps_aggregate_initializing() {
        let mut tracker = ReadinessTracker::default();
        tracker.ingest(
            heartbeat("localize", api::presence::Readiness::Initializing),
            1_000_000,
        );

        assert_eq!(
            tracker.aggregate(1_000_000),
            api::presence::Readiness::Initializing
        );
    }

    #[test]
    fn ready_participants_are_ready() {
        let mut tracker = ReadinessTracker::default();
        tracker.ingest(
            heartbeat("drive", api::presence::Readiness::Ready),
            1_000_000,
        );
        tracker.ingest(
            heartbeat("safety", api::presence::Readiness::Ready),
            1_000_000,
        );

        assert_eq!(
            tracker.aggregate(1_000_000),
            api::presence::Readiness::Ready
        );
    }

    #[test]
    fn own_heartbeat_does_not_poison_aggregate() {
        let mut tracker = ReadinessTracker::default();
        tracker.ingest(
            heartbeat("presence", api::presence::Readiness::Degraded),
            1_000_000,
        );

        assert_eq!(
            tracker.aggregate(1_000_000),
            api::presence::Readiness::Ready
        );
    }

    #[test]
    fn stale_threshold_is_exclusive() {
        assert!(!is_stale(10 + STALE_NS, 10));
        assert!(is_stale(10 + STALE_NS + 1, 10));
    }

    #[test]
    fn emit_apis_reports_contracts() {
        let metadata = phoxal::participant::participant_metadata::<Presence>();
        assert_eq!(metadata.artifact.id, "presence");

        let contracts = metadata.required_contracts;
        assert!(
            contracts
                .iter()
                .any(|c| c.family == <api::presence::Heartbeat as ContractBody>::FAMILY)
        );
        assert!(
            contracts
                .iter()
                .any(|c| c.family == <api::presence::State as ContractBody>::FAMILY)
        );
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
