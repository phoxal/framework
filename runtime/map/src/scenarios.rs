use std::borrow::Cow;
use std::time::Instant;

use crate::core::occupancy::{GRID_HEIGHT_CELLS, GRID_WIDTH_CELLS, OccupancyGrid};
use crate::core::revisions::{
    INITIAL_MAP_EPOCH, RETAIN_COMPLETED_REVISIONS, RevisionLookup, RevisionStore,
};
use crate::core::submaps::SubmapStore;
use anyhow::{Result, bail, ensure};
use phoxal_api_localize::v1::{LocalizationMode, LocalizationRevisionId};
use phoxal_api_map::v1::{
    MapRevisionCause, MapRevisionId, Traversability, TraversabilityCell, TraversabilityStatus,
};
use phoxal_api_mission::v1::{GoalPose, GoalTolerance};
use phoxal_api_motion::v1::ManualCommand;
use phoxal_core_engine::RobotRuntimeArgs;
use phoxal_core_engine::step::{ScenarioDescriptor, ScenarioKind};
use phoxal_validation_scenario::harness::ScenarioContext;
use phoxal_validation_scenario::helpers::{
    assert_close, assert_schema, keyframe, localization_revision,
};
use phoxal_validation_scenario::webots::{
    command_deadline, context_from_args, publish_and_advance, wait_until_tracking,
};

const ORB_COMMAND_STEP_SECS: f64 = 0.25;

pub const SCENARIOS: &[ScenarioDescriptor] = &[
    ScenarioDescriptor {
        name: Cow::Borrowed("p2-mapping-revision-linkage"),
        summary: Cow::Borrowed("Checks map revisions link to localization revisions."),
        kind: ScenarioKind::Headless,
        phase: phoxal_core_engine::step::Phase::P2,
        timeout_secs: 60,
        category: Cow::Borrowed("mapping"),
        tier: 1,
    },
    ScenarioDescriptor {
        name: Cow::Borrowed("p2-traversability-body-envelope"),
        summary: Cow::Borrowed("Checks body-envelope inflation against fixture structure."),
        kind: ScenarioKind::Headless,
        phase: phoxal_core_engine::step::Phase::P2,
        timeout_secs: 60,
        category: Cow::Borrowed("traversability"),
        tier: 1,
    },
    ScenarioDescriptor {
        name: Cow::Borrowed("p2-revision-convergence-store"),
        summary: Cow::Borrowed("Checks map revision-store convergence and reset behavior."),
        kind: ScenarioKind::Headless,
        phase: phoxal_core_engine::step::Phase::P2,
        timeout_secs: 60,
        category: Cow::Borrowed("revision-convergence"),
        tier: 1,
    },
    ScenarioDescriptor {
        name: Cow::Borrowed("p2-mapping-submap-rgbd"),
        summary: Cow::Borrowed("Checks Webots RGB-D submap activation."),
        kind: ScenarioKind::Webots {
            world: Cow::Borrowed("ArenaWorld"),
        },
        phase: phoxal_core_engine::step::Phase::P2,
        timeout_secs: 120,
        category: Cow::Borrowed("mapping"),
        tier: 2,
    },
    ScenarioDescriptor {
        name: Cow::Borrowed("p2-traversability-depth-cell-evaluation"),
        summary: Cow::Borrowed("Checks Webots depth evidence reaches traversability."),
        kind: ScenarioKind::Webots {
            world: Cow::Borrowed("ArenaWorld"),
        },
        phase: phoxal_core_engine::step::Phase::P2,
        timeout_secs: 120,
        category: Cow::Borrowed("traversability"),
        tier: 2,
    },
    ScenarioDescriptor {
        name: Cow::Borrowed("p2-mapping-orb-driven-chain"),
        summary: Cow::Borrowed("Checks the ORB-driven map chain in Webots."),
        kind: ScenarioKind::Webots {
            world: Cow::Borrowed("MappingLoopArena"),
        },
        phase: phoxal_core_engine::step::Phase::P2,
        timeout_secs: 240,
        category: Cow::Borrowed("mapping"),
        tier: 2,
    },
];

pub async fn run(name: &str, common: &RobotRuntimeArgs) -> Result<()> {
    match name {
        "p2-mapping-revision-linkage" => p2_mapping_revision_linkage(),
        "p2-traversability-body-envelope" => p2_traversability_body_envelope(common),
        "p2-revision-convergence-store" => p2_revision_convergence_store(),
        "p2-mapping-submap-rgbd" => {
            let ctx = webots_context(common).await?;
            assert_p2_mapping(&ctx, deadline_for(name)?).await
        }
        "p2-traversability-depth-cell-evaluation" => {
            let ctx = webots_context(common).await?;
            assert_p2_traversability(&ctx, deadline_for(name)?).await
        }
        "p2-mapping-orb-driven-chain" => {
            let ctx = webots_context(common).await?;
            assert_p2_mapping_orb_driven_chain(&ctx, deadline_for(name)?).await
        }
        _ => anyhow::bail!("map has no scenario '{name}'"),
    }
}

async fn webots_context(common: &RobotRuntimeArgs) -> Result<ScenarioContext> {
    let ctx = context_from_args(common).await?;
    ctx.reset_simulation().await?;
    Ok(ctx)
}

fn deadline_for(name: &str) -> Result<Instant> {
    let timeout_secs = SCENARIOS
        .iter()
        .find(|scenario| scenario.name.as_ref() == name)
        .map(|scenario| scenario.timeout_secs)
        .unwrap_or(60);
    command_deadline(timeout_secs)
}

fn p2_mapping_revision_linkage() -> Result<()> {
    let mut store = RevisionStore::new();
    let mut previous_map_revision_id = None;
    let mut previous_localize_revision_id = None;

    for sequence in 0_u64..5 {
        let revision = localization_revision(1, sequence, previous_localize_revision_id);
        let Some(observed) = store.observe(&revision) else {
            bail!("fresh localization revision {sequence} did not emit a map revision");
        };

        ensure!(
            observed.map_revision_id
                == MapRevisionId {
                    epoch: INITIAL_MAP_EPOCH,
                    sequence,
                },
            "map revision sequence did not track localization revision sequence"
        );
        ensure!(
            observed.previous_map_revision_id == previous_map_revision_id,
            "map revision {sequence} did not link to the previous map revision"
        );
        let expected_cause = if sequence == 0 {
            MapRevisionCause::SensorIntegration
        } else {
            MapRevisionCause::LocalizationCorrection
        };
        ensure!(
            observed.cause == expected_cause,
            "map revision {sequence} cause drifted: expected {expected_cause:?}, got {:?}",
            observed.cause
        );

        previous_map_revision_id = Some(observed.map_revision_id);
        previous_localize_revision_id = Some(revision.revision_id);
    }

    ensure!(
        store.len() == RETAIN_COMPLETED_REVISIONS,
        "revision store retained {} revisions, expected {RETAIN_COMPLETED_REVISIONS}",
        store.len()
    );
    ensure!(
        store
            .current()
            .map(|revision| revision.map_revision_id.sequence)
            == Some(4),
        "current map revision should be sequence 4 after five observations"
    );
    ensure!(
        matches!(
            store.lookup(MapRevisionId {
                epoch: INITIAL_MAP_EPOCH,
                sequence: 0,
            }),
            RevisionLookup::Stale { .. }
        ),
        "evicted map revision 0 must be reported stale"
    );
    ensure!(
        matches!(
            store.lookup(MapRevisionId {
                epoch: INITIAL_MAP_EPOCH,
                sequence: 4,
            }),
            RevisionLookup::Found(_)
        ),
        "current map revision 4 must be found"
    );
    ensure!(
        matches!(
            store.lookup(MapRevisionId {
                epoch: 99,
                sequence: 0,
            }),
            RevisionLookup::WrongEpoch { .. }
        ),
        "wrong map epoch must be reported explicitly"
    );

    let mut submaps = SubmapStore::new();
    let keyframe = keyframe(
        "kf-4",
        LocalizationRevisionId {
            epoch: 1,
            sequence: 4,
        },
    );
    let Some(created) = submaps.ingest(&keyframe) else {
        bail!("first keyframe did not create a submap");
    };
    ensure!(
        created.built_from_localize_revision == keyframe.revision,
        "created submap did not retain its localization revision"
    );
    ensure!(
        submaps
            .latest()
            .map(|metadata| metadata.built_from_localize_revision)
            == Some(keyframe.revision),
        "latest submap did not report the keyframe localization revision"
    );

    Ok(())
}

fn p2_traversability_body_envelope(common: &RobotRuntimeArgs) -> Result<()> {
    const OCCUPANCY_FREE: u8 = 1;
    const OCCUPANCY_OCCUPIED: u8 = 2;

    let structure = common.structure()?;
    let body_radius =
        crate::core::body_envelope::body_radius_from_structure(&structure, "base_link")?;
    assert_close("body radius", body_radius, 0.291_547_594_742_265, 1e-9)?;

    let mut grid = OccupancyGrid::centered_at([0.0, 0.0]);
    let center_cell_index = cell_index(25, 25);
    grid.cells_mut().fill(OCCUPANCY_FREE);
    grid.cells_mut()[center_cell_index] = OCCUPANCY_OCCUPIED;
    let traversability = grid.traversability(body_radius);

    assert_eq!(
        traversability.cells[center_cell_index],
        TraversabilityCell::Occupied
    );
    assert_eq!(
        traversability.cells[center_cell_index + 1],
        TraversabilityCell::Inflated
    );
    assert_eq!(traversability.cells[0], TraversabilityCell::Free);

    assert_schema::<Traversability>("runtime/map/traversability", 1, "map traversability")
}

fn p2_revision_convergence_store() -> Result<()> {
    let mut store = RevisionStore::new();
    let mut previous_localize_revision_id = None;

    for sequence in 0_u64..3 {
        let revision = localization_revision(5, sequence, previous_localize_revision_id);
        let Some(observed) = store.observe(&revision) else {
            bail!("fresh same-epoch localization revision {sequence} did not emit a map revision");
        };
        ensure!(
            observed.map_revision_id.sequence == sequence,
            "same-epoch map sequence did not advance with localization sequence"
        );
        previous_localize_revision_id = Some(revision.revision_id);
    }

    ensure!(
        store.epoch() == INITIAL_MAP_EPOCH,
        "same localization epoch observations must not advance the map epoch"
    );
    ensure!(
        store.len() == 3,
        "same localization epoch observations should retain three revisions"
    );
    ensure!(
        store
            .current()
            .map(|revision| revision.map_revision_id.sequence)
            == Some(2),
        "current same-epoch map revision should be sequence 2"
    );

    let reset_revision = localization_revision(6, 0, None);
    let Some(reset) = store.observe(&reset_revision) else {
        bail!("new localization epoch did not reset the map revision store");
    };
    ensure!(
        reset.cause == MapRevisionCause::Reset,
        "new localization epoch should emit a reset map revision"
    );
    ensure!(
        reset.previous_map_revision_id.is_none(),
        "reset map revision must not link to the previous map epoch"
    );
    ensure!(
        store.epoch() == INITIAL_MAP_EPOCH + 1,
        "new localization epoch did not advance the map epoch"
    );
    ensure!(
        store.len() == 1,
        "map revision retention should clear on reset"
    );
    ensure!(
        store
            .current()
            .map(|revision| revision.map_revision_id.sequence)
            == Some(0),
        "current reset map revision should restart at sequence 0"
    );

    let stale_revision = localization_revision(5, 99, None);
    ensure!(
        store.observe(&stale_revision).is_none(),
        "older localization epoch should be skipped as stale"
    );
    ensure!(
        store.epoch() == INITIAL_MAP_EPOCH + 1
            && store.len() == 1
            && store
                .current()
                .map(|revision| revision.map_revision_id.sequence)
                == Some(0),
        "stale localization epoch changed map revision store state"
    );

    Ok(())
}

async fn assert_p2_mapping(ctx: &ScenarioContext, deadline: Instant) -> Result<()> {
    wait_until_tracking(ctx, deadline).await?;

    let summary = ctx.latest_map_summary().await?;
    ensure!(
        summary.data.current_revision.is_some(),
        "map did not activate (no current_revision)"
    );
    Ok(())
}

async fn assert_p2_traversability(ctx: &ScenarioContext, deadline: Instant) -> Result<()> {
    wait_until_tracking(ctx, deadline).await?;

    ctx.publish_navigate_to(
        GoalPose::Pose2 {
            frame_id: "map".into(),
            map_revision: None,
            xy_m: [0.6, 0.0],
            yaw_rad: 0.0,
        },
        GoalTolerance {
            pos_m: 0.20,
            yaw_rad: Some(0.14),
        },
    )
    .await?;
    ctx.advance_for_secs(6.0).await?;

    let summary = ctx.latest_traversability_summary().await?;
    ensure!(
        summary.data.status == TraversabilityStatus::Ready,
        "traversability not ready after sensor evidence, got {:?}",
        summary.data.status
    );
    Ok(())
}

async fn assert_p2_mapping_orb_driven_chain(
    ctx: &ScenarioContext,
    deadline: Instant,
) -> Result<()> {
    const FORWARD_MPS: f64 = 0.18;
    const TURN_RADPS: f64 = 1.3;
    const SAFE_RADIUS_M: f64 = 1.8;

    let start = ctx.simulation_pose().await?.data;
    let start_xy = [start.translation_m[0], start.translation_m[1]];

    loop {
        let truth = ctx.simulation_pose().await?.data;
        let drift_m = ((truth.translation_m[0] - start_xy[0]).powi(2)
            + (truth.translation_m[1] - start_xy[1]).powi(2))
        .sqrt();
        ensure!(
            drift_m <= SAFE_RADIUS_M,
            "ORB-driven map robot left the clear center ({drift_m:.2} m from start, limit {SAFE_RADIUS_M} m)"
        );

        publish_and_advance(
            ctx,
            ManualCommand {
                linear_x_mps: FORWARD_MPS,
                angular_z_radps: TURN_RADPS,
            },
            ORB_COMMAND_STEP_SECS,
        )
        .await?;

        let localize = ctx.latest_localization_state().await?;
        if localize.data.mode == LocalizationMode::Tracking {
            let summary = ctx.latest_map_summary().await?;
            let traversability = ctx.latest_traversability_summary().await?;
            let occupancy_ready = traversability.data.status == TraversabilityStatus::Ready;
            let loop_closed = matches!(
                summary.data.built_from_localize_revision,
                Some(revision) if revision.sequence > 0
            );
            if occupancy_ready && loop_closed {
                return Ok(());
            }
        }

        ensure!(
            Instant::now() < deadline,
            "ORB-driven map chain did not converge (mode {:?}, built_from_localize_revision {:?})",
            localize.data.mode,
            ctx.latest_map_summary()
                .await?
                .data
                .built_from_localize_revision
        );
    }
}

fn cell_index(x_cell: u32, y_cell: u32) -> usize {
    debug_assert!(x_cell < GRID_WIDTH_CELLS);
    debug_assert!(y_cell < GRID_HEIGHT_CELLS);
    (y_cell * GRID_WIDTH_CELLS + x_cell) as usize
}
