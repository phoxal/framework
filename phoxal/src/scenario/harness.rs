use std::collections::BTreeMap;
use std::time::Duration;

use crate::api::mission::v1::{
    ExplorationCompletion, ExplorationCompletionMode, GoalPose, GoalTolerance, MissionCommand,
};
use crate::api::{
    explore, follow, localize, map, mission, plan, presence, safety,
    simulation::{clock, pose, reset, status},
    topic,
};
use crate::bus::Bus;
use crate::bus::typed::{Received, TypedTopicSubscriber};
use crate::runtime::DEFAULT_ROBOT_NAMESPACE;
use anyhow::{Context, Result, anyhow, bail};
use serde::de::DeserializeOwned;

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
        let robot_router_endpoint =
            var(crate::runtime::ENV_ROBOT_ROUTER_ENDPOINT).with_context(|| {
                format!(
                    "{} must be set for scenario context",
                    crate::runtime::ENV_ROBOT_ROUTER_ENDPOINT
                )
            })?;
        let robot_id = var(crate::runtime::ENV_ROBOT_ID).with_context(|| {
            format!(
                "{} must be set for scenario context",
                crate::runtime::ENV_ROBOT_ID
            )
        })?;
        let robot_namespace = var(crate::runtime::ENV_ROBOT_NAMESPACE)
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
                crate::runtime::ENV_ROBOT_ROUTER_ENDPOINT,
            )?,
            robot_namespace: trim_required(
                robot_namespace.into(),
                crate::runtime::ENV_ROBOT_NAMESPACE,
            )?,
            robot_id: trim_required(robot_id.into(), crate::runtime::ENV_ROBOT_ID)?,
        };
        Ok(environment)
    }

    pub async fn connect(self, wallclock_timeout: Duration) -> Result<ScenarioContext> {
        let bus = crate::bus::builder::Builder::new(self.robot_router_endpoint.clone())
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
            crate::bus::query::Retry::new(3).with_initial_backoff(Duration::from_millis(50));
        let response = self
            .bus
            .request(
                &topic::new().simulation().reset(),
                &reset::Request::V1(reset::v1::Request),
                &retry,
            )
            .await?
            .ok_or_else(|| anyhow!("simulation reset returned no acknowledgement"))?;
        let reset::Response::V1(response) = response;
        self.wait_for_status(|status| status.epoch >= response.epoch && status.step > 0)
            .await?;
        Ok(response.epoch)
    }

    pub async fn wait_until_ready(&self) -> Result<Received<status::Status>> {
        self.wait_for_status(|status| status.step > 0).await
    }

    pub async fn advance_for_secs(&self, secs: f64) -> Result<Received<clock::Clock>> {
        let duration_ns = duration_ns_from_secs(secs)?;
        let subscriber = self
            .bus
            .subscriber(&topic::new().simulation().clock())
            .await?;
        let first = next_received(&subscriber, self.wallclock_timeout).await?;
        let first_tick = clock_v1(&first.value);
        let epoch = first_tick.epoch();
        let target_time_ns = first_tick
            .time_ns()
            .checked_add(duration_ns)
            .ok_or_else(|| anyhow!("scenario target logical time overflows nanoseconds"))?;
        let mut latest = first;

        while clock_v1(&latest.value).time_ns() < target_time_ns {
            latest = next_received(&subscriber, self.wallclock_timeout).await?;
            let latest_tick = clock_v1(&latest.value);
            if latest_tick.epoch() != epoch {
                bail!(
                    "simulation epoch changed from {} to {} while waiting for logical time",
                    epoch,
                    latest_tick.epoch()
                );
            }
        }

        Ok(latest)
    }

    pub async fn simulation_pose(&self) -> Result<Received<pose::Pose>> {
        let subscriber = self
            .bus
            .subscriber(
                &topic::new()
                    .simulation()
                    .robot(self.environment.robot_id.clone())
                    .pose(),
            )
            .await?;
        next_received(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_localization_state(&self) -> Result<Received<localize::LocalizationState>> {
        let subscriber = self
            .bus
            .subscriber(&topic::new().localize().state())
            .await?;
        next_received(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_presence_summary(&self) -> Result<Received<presence::Summary>> {
        let subscriber = self
            .bus
            .subscriber(&topic::new().presence().summary())
            .await?;
        next_received(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_safety_state(&self) -> Result<Received<safety::State>> {
        let subscriber = self.bus.subscriber(&topic::new().safety().state()).await?;
        next_received(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_plan_state(&self) -> Result<Received<plan::State>> {
        let subscriber = self.bus.subscriber(&topic::new().plan().state()).await?;
        next_received(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_follow_state(&self) -> Result<Received<follow::State>> {
        let subscriber = self.bus.subscriber(&topic::new().follow().state()).await?;
        next_received(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_mission_state(&self) -> Result<Received<mission::State>> {
        let subscriber = self.bus.subscriber(&topic::new().mission().state()).await?;
        next_received(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_explore_state(&self) -> Result<Received<explore::State>> {
        let subscriber = self.bus.subscriber(&topic::new().explore().state()).await?;
        next_received(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_explore_candidates(&self) -> Result<Received<explore::GoalCandidates>> {
        let subscriber = self
            .bus
            .subscriber(&topic::new().explore().goal_candidates())
            .await?;
        next_received(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_map_summary(&self) -> Result<Received<map::Summary>> {
        let subscriber = self.bus.subscriber(&topic::new().map().summary()).await?;
        next_received(&subscriber, self.wallclock_timeout).await
    }

    pub async fn latest_traversability_summary(
        &self,
    ) -> Result<Received<map::TraversabilitySummary>> {
        let subscriber = self
            .bus
            .subscriber(&topic::new().map().traversability_summary())
            .await?;
        next_received(&subscriber, self.wallclock_timeout).await
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
        command: crate::api::motion::ManualCommand,
    ) -> Result<()> {
        let ready = self.wait_until_ready().await?;
        let produced_at_ns = status_v1(&ready.value).time_ns;
        self.bus
            .publish(&topic::new().motion().manual(), produced_at_ns, &command)
            .await
            .map_err(Into::into)
    }

    async fn wait_for_status(
        &self,
        predicate: impl Fn(&status::v1::Status) -> bool,
    ) -> Result<Received<status::Status>> {
        let subscriber = self
            .bus
            .subscriber(&topic::new().simulation().status())
            .await?;
        loop {
            let status = next_received(&subscriber, self.wallclock_timeout).await?;
            if predicate(status_v1(&status.value)) {
                return Ok(status);
            }
        }
    }

    async fn publish_mission_command(&self, command: MissionCommand) -> Result<()> {
        let ready = self.wait_until_ready().await?;
        let produced_at_ns = status_v1(&ready.value).time_ns;
        self.bus
            .publish(
                &topic::new().mission().command(),
                produced_at_ns,
                &mission::MissionCommand::V1(command),
            )
            .await
            .map_err(Into::into)
    }
}

fn clock_v1(value: &clock::Clock) -> &clock::v1::Clock {
    match value {
        clock::Clock::V1(value) => value,
    }
}

fn status_v1(value: &status::Status) -> &status::v1::Status {
    match value {
        status::Status::V1(value) => value,
    }
}

async fn next_received<T>(
    subscriber: &TypedTopicSubscriber<T>,
    timeout: Duration,
) -> Result<Received<T>>
where
    T: DeserializeOwned,
{
    match tokio::time::timeout(timeout, subscriber.recv()).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(anyhow!("scenario subscriber failed: {error}")),
        Err(_) => bail!("timed out waiting for scenario data after {:?}", timeout),
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
                crate::runtime::ENV_ROBOT_ROUTER_ENDPOINT.to_string(),
                "tcp/127.0.0.1:7447".to_string(),
            ),
            (
                crate::runtime::ENV_ROBOT_NAMESPACE.to_string(),
                "dev".to_string(),
            ),
            (
                crate::runtime::ENV_ROBOT_ID.to_string(),
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
            crate::runtime::ENV_ROBOT_ID.to_string(),
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
