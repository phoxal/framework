//! `mission` - lifecycle owner for autonomy goals.
//!
//! This runtime is deliberately not a drive controller. It subscribes to
//! `mission/command` (Start/Pause/Resume/Cancel), latches the active goal, and
//! publishes `mission/goal` (only while Active) plus the `mission/state`
//! lifecycle. Pause/Resume/Cancel keep or clear the latched goal; an invalid
//! Pause or Resume is reported in the state `detail` without changing phase.
//!
//! Because this first API slice does not feed arrival/completion observations
//! into `mission`, an active goal remains active until Pause, Resume, Cancel, or a
//! replacement Start command is received.

use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

#[derive(Clone, Debug, PartialEq)]
struct MissionLifecycle {
    phase: api::mission::Phase,
    goal: Option<api::mission::Goal>,
    detail: Option<String>,
}

impl MissionLifecycle {
    fn idle() -> Self {
        Self {
            phase: api::mission::Phase::Idle,
            goal: None,
            detail: None,
        }
    }

    fn apply_command(&mut self, command: api::mission::Command) {
        match command {
            api::mission::Command::Start(goal) => {
                self.phase = api::mission::Phase::Active;
                self.goal = Some(goal);
                self.detail = None;
            }
            api::mission::Command::Pause => self.pause(),
            api::mission::Command::Resume => self.resume(),
            api::mission::Command::Cancel => {
                self.phase = api::mission::Phase::Idle;
                self.goal = None;
                self.detail = None;
            }
        }
    }

    fn pause(&mut self) {
        match self.phase {
            api::mission::Phase::Active => {
                self.phase = api::mission::Phase::Paused;
                self.detail = None;
            }
            api::mission::Phase::Paused => {
                self.detail = None;
            }
            api::mission::Phase::Idle
            | api::mission::Phase::Succeeded
            | api::mission::Phase::Failed => {
                self.detail = Some("pause ignored: no active mission".to_string());
            }
        }
    }

    fn resume(&mut self) {
        match (self.phase, self.goal.is_some()) {
            (api::mission::Phase::Paused, true) => {
                self.phase = api::mission::Phase::Active;
                self.detail = None;
            }
            (api::mission::Phase::Active, _) => {
                self.detail = None;
            }
            _ => {
                self.detail = Some("resume ignored: no paused goal".to_string());
            }
        }
    }

    fn goal_to_publish(&self) -> Option<api::mission::Goal> {
        (self.phase == api::mission::Phase::Active)
            .then(|| self.goal.clone())
            .flatten()
    }

    fn state(&self) -> api::mission::State {
        api::mission::State {
            phase: self.phase,
            goal: self.goal.clone(),
            detail: self.detail.clone(),
        }
    }
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "mission", api = y2026_1)]
struct Mission {
    lifecycle: MissionLifecycle,
    command: Subscriber<api::mission::Command>,
    goal: Publisher<api::mission::Goal>,
    state: Publisher<api::mission::State>,
}

#[phoxal::runtime]
impl Mission {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        Ok(Self {
            lifecycle: MissionLifecycle::idle(),
            command: ctx
                .subscribe(api::topic::internal::new(cap).mission().command())
                .subscriber()
                .await?,
            goal: ctx
                .publisher(api::topic::internal::new(cap).mission().goal())
                .await?,
            state: ctx
                .publisher(api::topic::internal::new(cap).mission().state())
                .await?,
        })
    }

    #[step(hz = 5)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        while let Some(received) = self.command.try_recv() {
            self.lifecycle.apply_command(received.body);
        }

        if let Some(goal) = self.lifecycle.goal_to_publish() {
            self.goal.publish_at(step.time(), goal).await?;
        }
        self.state
            .publish_at(step.time(), self.lifecycle.state())
            .await?;
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Mission>()
}

#[cfg(test)]
mod tests {
    use phoxal_api::ContractBody;
    use phoxal_api::y2026_1 as api;

    use super::{Mission, MissionLifecycle};

    #[test]
    fn start_latches_active_goal() {
        let goal = goal(1.0, 2.0);
        let mut lifecycle = MissionLifecycle::idle();

        lifecycle.apply_command(api::mission::Command::Start(goal.clone()));

        assert_eq!(lifecycle.phase, api::mission::Phase::Active);
        assert_eq!(lifecycle.goal, Some(goal.clone()));
        assert_eq!(lifecycle.goal_to_publish(), Some(goal));
        assert!(lifecycle.detail.is_none());
    }

    #[test]
    fn pause_and_resume_keep_latched_goal() {
        let goal = goal(0.5, -1.0);
        let mut lifecycle = MissionLifecycle::idle();
        lifecycle.apply_command(api::mission::Command::Start(goal.clone()));

        lifecycle.apply_command(api::mission::Command::Pause);
        assert_eq!(lifecycle.phase, api::mission::Phase::Paused);
        assert_eq!(lifecycle.goal, Some(goal.clone()));
        assert_eq!(lifecycle.goal_to_publish(), None);

        lifecycle.apply_command(api::mission::Command::Resume);
        assert_eq!(lifecycle.phase, api::mission::Phase::Active);
        assert_eq!(lifecycle.goal_to_publish(), Some(goal));
    }

    #[test]
    fn cancel_returns_to_idle_and_clears_goal() {
        let mut lifecycle = MissionLifecycle::idle();
        lifecycle.apply_command(api::mission::Command::Start(goal(1.0, 0.0)));

        lifecycle.apply_command(api::mission::Command::Cancel);

        assert_eq!(lifecycle.phase, api::mission::Phase::Idle);
        assert_eq!(lifecycle.goal, None);
        assert_eq!(lifecycle.goal_to_publish(), None);
        assert!(lifecycle.detail.is_none());
    }

    #[test]
    fn invalid_pause_or_resume_is_reported_without_phase_change() {
        let mut lifecycle = MissionLifecycle::idle();

        lifecycle.apply_command(api::mission::Command::Pause);
        assert_eq!(lifecycle.phase, api::mission::Phase::Idle);
        assert!(
            lifecycle
                .detail
                .as_deref()
                .unwrap()
                .contains("pause ignored")
        );

        lifecycle.apply_command(api::mission::Command::Resume);
        assert_eq!(lifecycle.phase, api::mission::Phase::Idle);
        assert!(
            lifecycle
                .detail
                .as_deref()
                .unwrap()
                .contains("resume ignored")
        );
    }

    #[test]
    fn state_mirrors_lifecycle() {
        let goal = goal(2.0, 3.0);
        let mut lifecycle = MissionLifecycle::idle();
        lifecycle.apply_command(api::mission::Command::Start(goal.clone()));

        let state = lifecycle.state();

        assert_eq!(state.phase, api::mission::Phase::Active);
        assert_eq!(state.goal, Some(goal));
        assert_eq!(state.detail, None);
    }

    #[test]
    fn emit_apis_reports_mission_lifecycle_contracts() {
        let json = phoxal::runtime::emit_apis_json::<Mission>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "mission");
        assert_eq!(value["api_version"], "y2026_1");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert_contract::<api::mission::Command>(contracts, "subscribe");
        assert_contract::<api::mission::Goal>(contracts, "publish");
        assert_contract::<api::mission::State>(contracts, "publish");
        assert!(!contracts.iter().any(|c| {
            c["family"] == <api::drive::Target as ContractBody>::FAMILY
                && c["direction"] == "publish"
        }));
    }

    fn assert_contract<B>(contracts: &[serde_json::Value], direction: &str)
    where
        B: ContractBody,
    {
        assert!(contracts.iter().any(|c| {
            c["family"] == B::FAMILY && c["topic"] == B::TOPIC && c["direction"] == direction
        }));
    }

    fn goal(x_m: f64, y_m: f64) -> api::mission::Goal {
        api::mission::Goal {
            x_m,
            y_m,
            yaw_rad: Some(0.25),
        }
    }
}
