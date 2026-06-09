use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use phoxal::api::presence::{
    DebugReadiness, Heartbeat, Readiness, RuntimeId, RuntimeReadiness, Summary, debug, heartbeat,
    summary,
};
use phoxal::bus::pubsub::Stamped;
use phoxal::runtime::clock::Step;
use phoxal::runtime::stale_timeout_ns;
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};
use phoxal::runtime::{Io, Publisher, Runtime, RuntimeInputs};

const PUBLISH_HZ: f64 = 1.0;

fn presence_runtime_id() -> RuntimeId {
    RuntimeId::new("presence")
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Config;

impl Config {
    pub const fn clock_period(&self) -> Duration {
        Duration::from_secs(1)
    }
}

pub enum Input {
    Heartbeat(Stamped<Heartbeat>),
}

pub struct PresenceRuntime {
    tracker: ReadinessTracker,
    summary_pub: Publisher<Stamped<Summary>>,
    debug_readiness_pub: Publisher<Stamped<DebugReadiness>>,
}

#[async_trait::async_trait]
impl Runtime for PresenceRuntime {
    const RUNTIME_ID: &'static str = "presence";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, _common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Ok(Config)
    }

    fn clock_period(config: &Self::Config) -> Duration {
        config.clock_period()
    }

    async fn new(io: &mut Io<Self::Input>, _config: Self::Config) -> Result<Self> {
        io.subscribe::<Stamped<Heartbeat>, _>(&heartbeat::path(), Input::Heartbeat)
            .await?;
        let summary_pub = io.publisher::<Stamped<Summary>>(&summary::path()).await?;
        let debug_readiness_pub = io
            .publisher::<Stamped<DebugReadiness>>(&debug::readiness::path())
            .await?;

        Ok(Self {
            tracker: ReadinessTracker::default(),
            summary_pub,
            debug_readiness_pub,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        let timestamp_ns = step.tick.time_ns();

        for input in inputs {
            match input {
                Input::Heartbeat(stamped) => {
                    self.tracker.ingest(stamped.data, stamped.timestamp_ns)
                }
            }
        }

        let own = Heartbeat {
            runtime_id: presence_runtime_id(),
            readiness: Readiness::Ready,
        };
        self.tracker.ingest(own, timestamp_ns);

        let runtimes = self.tracker.snapshot(timestamp_ns);
        self.summary_pub
            .put(&Stamped::new(
                timestamp_ns,
                Summary {
                    autonomy_ready: autonomy_ready(&runtimes),
                    runtimes: runtimes.clone(),
                },
            ))
            .await?;
        self.debug_readiness_pub
            .put(&Stamped::new(timestamp_ns, DebugReadiness { runtimes }))
            .await?;

        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::ScenarioDescriptor] {
        crate::scenarios::SCENARIOS
    }

    async fn run_scenario(
        name: &str,
        _common: &RobotRuntimeArgs,
        _args: &Self::Args,
    ) -> Result<()> {
        crate::scenarios::run(name)
    }
}

#[derive(Debug, Default)]
struct ReadinessTracker {
    runtimes: HashMap<RuntimeId, RuntimeRecord>,
}

fn autonomy_ready(runtimes: &[RuntimeReadiness]) -> bool {
    !runtimes.is_empty()
        && runtimes
            .iter()
            .all(|runtime| runtime.readiness == Readiness::Ready)
}

impl ReadinessTracker {
    fn ingest(&mut self, heartbeat: Heartbeat, last_seen_ns: u64) {
        self.runtimes.insert(
            heartbeat.runtime_id,
            RuntimeRecord {
                readiness: heartbeat.readiness,
                last_seen_ns,
            },
        );
    }

    fn snapshot(&self, now_ns: u64) -> Vec<RuntimeReadiness> {
        let mut runtimes = self
            .runtimes
            .iter()
            .map(|(runtime_id, record)| RuntimeReadiness {
                runtime_id: runtime_id.clone(),
                readiness: if is_stale(now_ns, record.last_seen_ns) {
                    Readiness::Degraded
                } else {
                    record.readiness
                },
            })
            .collect::<Vec<_>>();
        runtimes.sort_by(|left, right| left.runtime_id.0.cmp(&right.runtime_id.0));
        runtimes
    }
}

#[derive(Debug)]
struct RuntimeRecord {
    readiness: Readiness,
    last_seen_ns: u64,
}

fn is_stale(now_ns: u64, last_seen_ns: u64) -> bool {
    now_ns.saturating_sub(last_seen_ns) > stale_timeout_ns(PUBLISH_HZ)
}

#[cfg(test)]
mod tests {
    use super::{ReadinessTracker, autonomy_ready};
    use phoxal::api::presence::{Heartbeat, Readiness, RuntimeId, RuntimeReadiness};
    use phoxal::runtime::stale_timeout_ns;

    #[test]
    fn stale_runtime_is_reported_degraded() {
        let mut tracker = ReadinessTracker::default();
        let last_seen_ns = 1_000_000_000;
        tracker.ingest(heartbeat("drive", Readiness::Ready), last_seen_ns);

        let snapshot = tracker.snapshot(last_seen_ns + stale_timeout_ns(1.0) + 1);

        assert_eq!(
            snapshot,
            vec![RuntimeReadiness {
                runtime_id: RuntimeId::new("drive"),
                readiness: Readiness::Degraded,
            }]
        );
    }

    #[test]
    fn fresh_runtime_keeps_reported_readiness() {
        let mut tracker = ReadinessTracker::default();
        let now_ns = 3_000_000_000;
        tracker.ingest(
            heartbeat("safety", Readiness::Ready),
            now_ns - stale_timeout_ns(1.0),
        );

        let snapshot = tracker.snapshot(now_ns);

        assert_eq!(
            snapshot,
            vec![RuntimeReadiness {
                runtime_id: RuntimeId::new("safety"),
                readiness: Readiness::Ready,
            }]
        );
    }

    #[test]
    fn initializing_runtime_round_trips() {
        let mut tracker = ReadinessTracker::default();
        let now_ns = 5_000_000_000;
        tracker.ingest(heartbeat("localize", Readiness::Initializing), now_ns);

        let snapshot = tracker.snapshot(now_ns);

        assert_eq!(snapshot[0].readiness, Readiness::Initializing);
    }

    #[test]
    fn failed_runtime_round_trips() {
        let mut tracker = ReadinessTracker::default();
        let now_ns = 5_000_000_000;
        tracker.ingest(heartbeat("plan", Readiness::Failed), now_ns);

        let snapshot = tracker.snapshot(now_ns);

        assert_eq!(snapshot[0].readiness, Readiness::Failed);
    }

    #[test]
    fn autonomy_ready_is_false_for_empty_snapshot() {
        assert!(!autonomy_ready(&[]));
    }

    #[test]
    fn autonomy_ready_is_true_when_all_runtimes_are_ready() {
        let runtimes = vec![
            runtime_readiness("presence", Readiness::Ready),
            runtime_readiness("drive", Readiness::Ready),
        ];

        assert!(autonomy_ready(&runtimes));
    }

    #[test]
    fn autonomy_ready_is_false_when_any_runtime_is_degraded() {
        let runtimes = vec![
            runtime_readiness("presence", Readiness::Ready),
            runtime_readiness("safety", Readiness::Degraded),
        ];

        assert!(!autonomy_ready(&runtimes));
    }

    #[test]
    fn autonomy_ready_is_false_when_any_runtime_is_failed() {
        let runtimes = vec![
            runtime_readiness("presence", Readiness::Ready),
            runtime_readiness("plan", Readiness::Failed),
        ];

        assert!(!autonomy_ready(&runtimes));
    }

    #[test]
    fn autonomy_ready_is_false_when_any_runtime_is_not_started() {
        let runtimes = vec![
            runtime_readiness("presence", Readiness::Ready),
            runtime_readiness("map", Readiness::NotStarted),
        ];

        assert!(!autonomy_ready(&runtimes));
    }

    fn heartbeat(runtime_id: &str, readiness: Readiness) -> Heartbeat {
        Heartbeat {
            runtime_id: RuntimeId::new(runtime_id),
            readiness,
        }
    }

    fn runtime_readiness(runtime_id: &str, readiness: Readiness) -> RuntimeReadiness {
        RuntimeReadiness {
            runtime_id: RuntimeId::new(runtime_id),
            readiness,
        }
    }
}
