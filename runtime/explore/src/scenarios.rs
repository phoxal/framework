use std::borrow::Cow;
use std::time::Instant;

use anyhow::{Result, ensure};
use phoxal::api::mission::v1::{GoalSource, MissionMode};
use phoxal::runtime::RobotRuntimeArgs;
use phoxal::runtime::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::harness::ScenarioContext;
use phoxal::scenario::webots::{
    command_deadline, context_from_args, wait_for_mission_state, wait_until_tracking,
};

pub const SCENARIOS: &[ScenarioDescriptor] = &[ScenarioDescriptor {
    name: Cow::Borrowed("p5-exploration-frontier"),
    summary: Cow::Borrowed("Checks exploration produces and follows a frontier goal in Webots."),
    kind: ScenarioKind::Webots {
        world: Cow::Borrowed("ArenaWorld"),
    },
    phase: phoxal::runtime::Phase::P5,
    timeout_secs: 180,
    category: Cow::Borrowed("exploration"),
    tier: 3,
}];

pub async fn run(name: &str, common: &RobotRuntimeArgs) -> Result<()> {
    match name {
        "p5-exploration-frontier" => {
            let ctx = context_from_args(common).await?;
            ctx.reset_simulation().await?;
            assert_p5_exploration_frontier(&ctx, deadline_for(name)?).await
        }
        _ => anyhow::bail!("explore has no scenario '{name}'"),
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

async fn assert_p5_exploration_frontier(ctx: &ScenarioContext, deadline: Instant) -> Result<()> {
    wait_until_tracking(ctx, deadline).await?;
    let start = ctx.simulation_pose().await?;
    let start_xy = [start.value.translation_m[0], start.value.translation_m[1]];

    ctx.publish_explore_command().await?;

    wait_for_mission_state(
        ctx,
        deadline,
        |state| state.mode == MissionMode::Exploring,
        "Exploring",
    )
    .await?;

    loop {
        let candidates = ctx.latest_explore_candidates().await?;
        if !candidates.value.candidates.is_empty() {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "explore produced no goal candidates"
        );
        ctx.advance_for_secs(0.5).await?;
    }

    wait_for_mission_state(
        ctx,
        deadline,
        |state| {
            state
                .active_goal
                .as_ref()
                .is_some_and(|goal| goal.source == GoalSource::Explore)
        },
        "an auto-promoted Explore goal",
    )
    .await?;

    loop {
        let pose = ctx.simulation_pose().await?;
        let dx = pose.value.translation_m[0] - start_xy[0];
        let dy = pose.value.translation_m[1] - start_xy[1];
        if (dx * dx + dy * dy).sqrt() >= 0.30 {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "robot did not drive toward an exploration frontier"
        );
        ctx.advance_for_secs(0.5).await?;
    }
}
