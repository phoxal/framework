use std::borrow::Cow;
use std::time::Instant;

use anyhow::{Result, anyhow, ensure};
use phoxal::api::v1::follow::FollowStatus;
use phoxal::runtime::RobotRuntimeArgs;
use phoxal::runtime::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::harness::ScenarioContext;
use phoxal::scenario::webots::{
    P4_GOAL_REACHED_TOLERANCE_M, P4_GOAL_XY_M, command_deadline, context_from_args, p4_goal,
    wait_until_robot_reaches, wait_until_tracking,
};

pub const SCENARIOS: &[ScenarioDescriptor] = &[ScenarioDescriptor {
    name: Cow::Borrowed("p4-following-revision-linked-path"),
    summary: Cow::Borrowed("Checks follow tracks the current revision-linked path in Webots."),
    kind: ScenarioKind::Webots {
        world: Cow::Borrowed("ArenaWorld"),
    },
    phase: phoxal::runtime::Phase::P4,
    timeout_secs: 120,
    category: Cow::Borrowed("following"),
    tier: 2,
}];

pub async fn run(name: &str, common: &RobotRuntimeArgs) -> Result<()> {
    match name {
        "p4-following-revision-linked-path" => {
            let ctx = context_from_args(common).await?;
            ctx.reset_simulation().await?;
            assert_p4_following(&ctx, deadline_for(name)?).await
        }
        _ => anyhow::bail!("follow has no scenario '{name}'"),
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

async fn assert_p4_following(ctx: &ScenarioContext, deadline: Instant) -> Result<()> {
    wait_until_tracking(ctx, deadline).await?;
    let (goal, tolerance) = p4_goal();
    ctx.publish_navigate_to(goal, tolerance).await?;

    loop {
        let state = ctx.latest_follow_state().await?;
        match state.value.status {
            FollowStatus::Tracking => break,
            FollowStatus::Failed | FollowStatus::Refused => {
                return Err(anyhow!(
                    "follow refused/failed: {:?} ({:?})",
                    state.value.status,
                    state.value.reason
                ));
            }
            _ => {}
        }
        ensure!(
            Instant::now() < deadline,
            "follow never started tracking (last {:?})",
            state.value.status
        );
        ctx.advance_for_secs(0.5).await?;
    }

    wait_until_robot_reaches(ctx, deadline, P4_GOAL_XY_M, P4_GOAL_REACHED_TOLERANCE_M).await
}
