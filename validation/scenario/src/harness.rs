use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use phoxal_api_explore::v1::{GoalCandidates, State as ExploreState};
use phoxal_api_follow::v1::State as FollowState;
use phoxal_api_localize::v1::LocalizationState;
use phoxal_api_map::v1::{Summary as MapSummary, TraversabilitySummary};
use phoxal_api_mission::v1::{
    ExplorationCompletion, ExplorationCompletionMode, GoalPose, GoalTolerance, MissionCommand,
    State as MissionState,
};
use phoxal_api_plan::v1::State as PlanState;
use phoxal_api_safety::v1::State as SafetyState;
use phoxal_core_engine::DEFAULT_ROBOT_NAMESPACE;
use phoxal_core_engine::presence::Summary;
use phoxal_core_engine::{
    sim_clock, sim_clock::SimulationClock as Clock, sim_pose, sim_pose::Pose, sim_reset as reset,
    sim_status, sim_status::Status,
};
use phoxal_infra_bus::Bus;
use phoxal_infra_bus::pubsub::Stamped;
use phoxal_infra_bus::zenoh_typed::{TypedPublisher, TypedSchema, TypedSubscriber};
use serde::{Serialize, de::DeserializeOwned};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ScenarioEnvironment {
    pub robot_router_endpoint: String,
    pub robot_namespace: String,
    pub robot_id: String,
}

pub struct ScenarioContext {
    bus: Bus,
    environment: ScenarioEnvironment,
    wallclock_timeout: Duration,
}

impl ScenarioEnvironment {
    pub fn from_env() -> Result<Self> {
        Self::from_vars(|key| std::env::var(key))
    }

    pub fn from_map(vars: &BTreeMap<String, String>) -> Result<Self> {
        Self::from_vars(|key| vars.get(key).cloned().ok_or(std::env::VarError::NotPresent))
    }

    fn from_vars(
        var: impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
    ) -> Result<Self> {
        let robot_router_endpoint = var(phoxal_core_engine::ENV_ROBOT_ROUTER_ENDPOINT)
            .with_context(|| {
                format!(
                    "{} must be set for scenario context",
                    phoxal_core_engine::ENV_ROBOT_ROUTER_ENDPOINT
                )
            })?;
        let robot_id = var(phoxal_core_engine::ENV_ROBOT_ID).with_context(|| {
            format!(
                "{} must be set for scenario context",
                phoxal_core_engine::ENV_ROBOT_ID
            )
        })?;
        let robot_namespace = var(phoxal_core_engine::ENV_ROBOT_NAMESPACE)
            .unwrap_or_else(|_| DEFAULT_ROBOT_NAMESPACE.to_string());

        Self::new(robot_router_endpoint, robot_namespace, robot_id)
    }

    pub fn new(
        robot_router_endpoint: impl Into<String>,
        robot_namespace: impl Into<String>,
        robot_id: impl Into<String>,
    ) -> Result<Self> {
        let environment = Self {
            robot_router_endpoint: trim_required(
                robot_router_endpoint.into(),
                phoxal_core_engine::ENV_ROBOT_ROUTER_ENDPOINT,
            )?,
            robot_namespace: trim_required(
                robot_namespace.into(),
                phoxal_core_engine::ENV_ROBOT_NAMESPACE,
            )?,
            robot_id: trim_required(robot_id.into(), phoxal_core_engine::ENV_ROBOT_ID)?,
        };
        Ok(environment)
    }

    pub async fn connect(self, wallclock_timeout: Duration) -> Result<ScenarioContext> {
        let bus = phoxal_infra_bus::builder::Builder::new(self.robot_router_endpoint.clone())
            .with_prefix(self.robot_namespace.clone())
            .connect()
            .await?;
        Ok(ScenarioContext {
            bus,
            environment: self,
            wallclock_timeout,
        })
    }
}

impl ScenarioContext {
    pub async fn from_env() -> Result<Self> {
        ScenarioEnvironment::from_env()
            .context("failed to build scenario environment from process env")?
            .connect(Duration::from_secs(30))
            .await
    }

    pub fn bus(&self) -> &Bus {
        &self.bus
    }

    pub fn environment(&self) -> &ScenarioEnvironment {
        &self.environment
    }

    pub async fn reset_simulation(&self) -> Result<u64> {
        let retry =
            phoxal_infra_bus::query::Retry::new(3).with_initial_backoff(Duration::from_millis(50));
        let response = reset::request(&self.bus, &reset::Request, &retry)
            .await?
            .ok_or_else(|| anyhow!("simulation reset returned no acknowledgement"))?;
        self.wait_for_status(|status| status.epoch >= response.epoch && status.step > 0)
            .await?;
        Ok(response.epoch)
    }

    pub async fn wait_until_ready(&self) -> Result<Stamped<Status>> {
        self.wait_for_status(|status| status.step > 0).await
    }

    pub async fn advance_for_secs(&self, secs: f64) -> Result<Stamped<Clock>> {
        let duration_ns = duration_ns_from_secs(secs)?;
        let subscriber = sim_clock::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        let first = next_stamped(&subscriber, self.wallclock_timeout).await?;
        let epoch = first.data.epoch();
        let target_time_ns = first
            .data
            .time_ns()
            .checked_add(duration_ns)
            .ok_or_else(|| anyhow!("scenario target logical time overflows nanoseconds"))?;
        let mut latest = first;

        while latest.data.time_ns() < target_time_ns {
            latest = next_stamped(&subscriber, self.wallclock_timeout).await?;
            if latest.data.epoch() != epoch {
                bail!(
                    "simulation epoch changed from {} to {} while waiting for logical time",
                    epoch,
                    latest.data.epoch()
                );
            }
        }

        Ok(latest)
    }

    pub async fn simulation_pose(&self) -> Result<Stamped<Pose>> {
        let subscriber = sim_pose::subscriber_builder(&self.bus, &self.environment.robot_id)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        next_stamped(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_localization_state(&self) -> Result<Stamped<LocalizationState>> {
        let subscriber = phoxal_api_localize::v1::state::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        next_stamped(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_presence_summary(&self) -> Result<Stamped<Summary>> {
        let subscriber = phoxal_core_engine::presence::summary::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        next_stamped(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_safety_state(&self) -> Result<Stamped<SafetyState>> {
        let subscriber = phoxal_api_safety::v1::state::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        next_stamped(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_plan_state(&self) -> Result<Stamped<PlanState>> {
        let subscriber = phoxal_api_plan::v1::state::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        next_stamped(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_follow_state(&self) -> Result<Stamped<FollowState>> {
        let subscriber = phoxal_api_follow::v1::state::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        next_stamped(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_mission_state(&self) -> Result<Stamped<MissionState>> {
        let subscriber = phoxal_api_mission::v1::state::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        next_stamped(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_explore_state(&self) -> Result<Stamped<ExploreState>> {
        let subscriber = phoxal_api_explore::v1::state::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        next_stamped(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_explore_candidates(&self) -> Result<Stamped<GoalCandidates>> {
        let subscriber = phoxal_api_explore::v1::goal_candidates::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        next_stamped(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_map_summary(&self) -> Result<Stamped<MapSummary>> {
        let subscriber = phoxal_api_map::v1::summary::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        next_stamped(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_traversability_summary(&self) -> Result<Stamped<TraversabilitySummary>> {
        let subscriber = phoxal_api_map::v1::traversability_summary::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        next_stamped(&subscriber, self.wallclock_timeout).await
    }

    pub async fn publish_navigate_to(
        &self,
        goal: GoalPose,
        tolerance: GoalTolerance,
    ) -> Result<()> {
        self.publish_mission_command(MissionCommand::NavigateTo {
            goal,
            tolerance,
            max_duration_ns: None,
        })
        .await
    }

    pub async fn publish_explore_command(&self) -> Result<()> {
        self.publish_mission_command(MissionCommand::Explore {
            area: None,
            completion: ExplorationCompletion {
                mode: ExplorationCompletionMode::OpenEnded,
                coverage_goal: None,
            },
            max_duration_ns: None,
        })
        .await
    }

    pub async fn publish_cancel(&self) -> Result<()> {
        self.publish_mission_command(MissionCommand::Cancel).await
    }

    pub async fn publish_pause(&self) -> Result<()> {
        self.publish_mission_command(MissionCommand::Pause).await
    }

    pub async fn publish_manual_command(
        &self,
        command: phoxal_api_motion::v1::ManualCommand,
    ) -> Result<()> {
        let produced_at_ns = self.wait_until_ready().await?.data.time_ns;
        let publisher = phoxal_api_motion::v1::manual::publisher(&self.bus)?
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        wait_for_matching_subscriber(&publisher, self.wallclock_timeout).await?;
        publisher
            .put(&Stamped::new(produced_at_ns, command))
            .await
            .map_err(|error| anyhow!(error.to_string()))
    }

    async fn wait_for_status(
        &self,
        predicate: impl Fn(&Status) -> bool,
    ) -> Result<Stamped<Status>> {
        let subscriber = sim_status::subscriber_builder(&self.bus)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        loop {
            let status = next_stamped(&subscriber, self.wallclock_timeout).await?;
            if predicate(&status.data) {
                return Ok(status);
            }
        }
    }

    async fn publish_mission_command(&self, command: MissionCommand) -> Result<()> {
        let produced_at_ns = self.wait_until_ready().await?.data.time_ns;
        let publisher = phoxal_api_mission::v1::command::publisher(&self.bus)?
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        wait_for_matching_subscriber(&publisher, self.wallclock_timeout).await?;
        publisher
            .put(&Stamped::new(produced_at_ns, command))
            .await
            .map_err(|error| anyhow!(error.to_string()))
    }
}

async fn next_stamped<T>(
    subscriber: &TypedSubscriber<Stamped<T>>,
    timeout: Duration,
) -> Result<Stamped<T>>
where
    T: DeserializeOwned + TypedSchema,
{
    match tokio::time::timeout(timeout, subscriber.recv_async()).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => Err(anyhow!("typed scenario payload failed to decode: {error}")),
        Ok(Err(error)) => Err(anyhow!("scenario subscriber failed: {error}")),
        Err(_) => bail!("timed out waiting for scenario data after {:?}", timeout),
    }
}

async fn wait_for_matching_subscriber<T>(
    publisher: &TypedPublisher<'_, T>,
    timeout: Duration,
) -> Result<()>
where
    T: Serialize + TypedSchema,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if publisher
            .has_matching_subscribers()
            .await
            .map_err(|error| anyhow!(error.to_string()))?
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("no subscriber matched the scenario command publisher within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn trim_required(value: String, name: &str) -> Result<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        bail!("{name} must not be empty");
    }
    Ok(value)
}

fn duration_ns_from_secs(secs: f64) -> Result<u64> {
    let duration = Duration::try_from_secs_f64(secs).map_err(|_| {
        anyhow!("scenario advance duration must be finite and non-negative, got {secs}")
    })?;
    duration
        .as_nanos()
        .try_into()
        .map_err(|_| anyhow!("scenario advance duration overflows nanoseconds: {secs} seconds"))
}

#[cfg(test)]
mod tests {
    use super::{ScenarioEnvironment, duration_ns_from_secs};
    use std::collections::BTreeMap;

    #[test]
    fn environment_uses_existing_runtime_env_names() -> anyhow::Result<()> {
        let vars = BTreeMap::from([
            (
                phoxal_core_engine::ENV_ROBOT_ROUTER_ENDPOINT.to_string(),
                "tcp/127.0.0.1:7447".to_string(),
            ),
            (
                phoxal_core_engine::ENV_ROBOT_NAMESPACE.to_string(),
                "dev".to_string(),
            ),
            (
                phoxal_core_engine::ENV_ROBOT_ID.to_string(),
                "robot-a".to_string(),
            ),
        ]);

        let env = ScenarioEnvironment::from_map(&vars)?;

        assert_eq!(env.robot_router_endpoint, "tcp/127.0.0.1:7447");
        assert_eq!(env.robot_namespace, "dev");
        assert_eq!(env.robot_id, "robot-a");
        Ok(())
    }

    #[test]
    fn environment_requires_router_endpoint() {
        let vars = BTreeMap::from([(
            phoxal_core_engine::ENV_ROBOT_ID.to_string(),
            "robot-a".to_string(),
        )]);

        assert!(ScenarioEnvironment::from_map(&vars).is_err());
    }

    #[test]
    fn duration_accepts_fractional_seconds() -> anyhow::Result<()> {
        assert_eq!(duration_ns_from_secs(1.5)?, 1_500_000_000);
        Ok(())
    }

    #[test]
    fn duration_rejects_invalid_seconds() {
        assert!(duration_ns_from_secs(f64::NAN).is_err());
        assert!(duration_ns_from_secs(-1.0).is_err());
    }
}
