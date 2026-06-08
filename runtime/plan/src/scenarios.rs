use std::borrow::Cow;
use std::time::Instant;

use anyhow::{Result, anyhow, ensure};
use phoxal::api::plan::v1::PlanStatus;
use phoxal::runtime::RobotRuntimeArgs;
use phoxal::runtime::step::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::harness::ScenarioContext;
use phoxal::scenario::webots::{command_deadline, context_from_args, p4_goal, wait_until_tracking};

pub const SCENARIOS: &[ScenarioDescriptor] = &[ScenarioDescriptor {
    name: Cow::Borrowed("p4-planning-goal-path"),
    summary: Cow::Borrowed("Checks a mission goal produces a plan path in Webots."),
    kind: ScenarioKind::Webots {
        world: Cow::Borrowed("ArenaWorld"),
    },
    phase: phoxal::runtime::step::Phase::P4,
    timeout_secs: 120,
    category: Cow::Borrowed("planning"),
    tier: 2,
}];

pub async fn run(name: &str, common: &RobotRuntimeArgs) -> Result<()> {
    match name {
        "p4-planning-goal-path" => {
            let ctx = context_from_args(common).await?;
            ctx.reset_simulation().await?;
            assert_p4_planning(&ctx, deadline_for(name)?).await
        }
        _ => anyhow::bail!("plan has no scenario '{name}'"),
    }
}

fn deadline_for(name: &str) -> Result<Instant> {
    let timeout_secs = SCENARIOS
        .iter()
        .find(|scenario| scenario.name.as_ref() == name)
        .map(|scenario| scenario.timeout_secs)
        .unwrap_or(60);
    command_deadline(timeout_secs)
}

async fn assert_p4_planning(ctx: &ScenarioContext, deadline: Instant) -> Result<()> {
    wait_until_tracking(ctx, deadline).await?;
    let (goal, tolerance) = p4_goal();
    ctx.publish_navigate_to(goal, tolerance).await?;

    loop {
        let state = ctx.latest_plan_state().await?;
        match state.data.status {
            PlanStatus::Ready => return Ok(()),
            PlanStatus::Failed | PlanStatus::Refused => {
                return Err(anyhow!(
                    "plan did not produce a path: {:?} ({:?})",
                    state.data.status,
                    state.data.reason
                ));
            }
            _ => {}
        }
        ensure!(
            Instant::now() < deadline,
            "plan never became Ready (last {:?})",
            state.data.status
        );
        ctx.advance_for_secs(0.5).await?;
    }
}
