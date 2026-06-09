use std::borrow::Cow;
use std::time::Instant;

use anyhow::{Result, anyhow, ensure};
use phoxal::api::localize::v1::{
    Keyframe, LocalizationMode, LocalizationRevision, LocalizationRevisionCause,
    LocalizationSource, LocalizationState, LocalizationStatusReason, PoseEstimate,
    PoseGraphCorrection,
};
use phoxal::api::map::v1::Summary as MapSummary;
use phoxal::api::motion::v1::ManualCommand;
use phoxal::api::simulation::v1::pose::Pose as SimulatorPose;
use phoxal::runtime::RobotRuntimeArgs;
use phoxal::runtime::runtime::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::harness::ScenarioContext;
use phoxal::scenario::helpers::{assert_schema, fixture_robot};
use phoxal::scenario::webots::{
    command_deadline, context_from_args, publish_and_advance, wait_until_tracking,
};

use crate::runtime::BackendSelection;

const ORB_TRACKING_BUDGET_SECS: f64 = 150.0;
const ORB_COMMAND_STEP_SECS: f64 = 0.25;
const ORB_WARMUP_SECS: f64 = 30.0;
const ORB_STABLE_WINDOW_SECS: f64 = 5.0;
const ORB_WANDER_FORWARD_MPS: f64 = 0.22;
const ORB_WANDER_TURN_MAX_RADPS: f64 = 0.5;
const ORB_WANDER_LEG_SECS: f64 = 2.0;
const ORB_STUCK_WINDOW_SECS: f64 = 1.5;
const ORB_STUCK_MIN_PROGRESS_M: f64 = 0.03;
const ORB_ESCAPE_BACKUP_MPS: f64 = -0.18;
const ORB_ESCAPE_BACKUP_SECS: f64 = 1.0;
const ORB_ESCAPE_SPIN_RADPS: f64 = 0.9;
const ORB_ESCAPE_SPIN_MIN_SECS: f64 = 1.5;
const ORB_ESCAPE_SPIN_MAX_SECS: f64 = 3.0;
const ORB_EXPLORER_SEED: u64 = 0x5DEE_CE66_D2B2_8C0F;
const ORB_MEASURE_SECS: f64 = 8.0;
const ORB_LOST_GRACE_SECS: f64 = 2.0;
const ORB_TRANSLATION_TOL_M: f64 = 0.30;
const ORB_YAW_TOL_RAD: f64 = 0.30;
const ORB_MIN_MEASURE_TRAVEL_M: f64 = 0.30;
const ORB_MIN_MEASURE_ROT_RAD: f64 = 0.30;

pub const SCENARIOS: &[ScenarioDescriptor] = &[
    ScenarioDescriptor {
        name: Cow::Borrowed("p2-localization-backend-conformance"),
        summary: Cow::Borrowed("Runs the localize backend conformance suite in-process."),
        kind: ScenarioKind::Headless,
        phase: phoxal::runtime::runtime::Phase::P2,
        timeout_secs: 60,
        category: Cow::Borrowed("localization"),
        tier: 1,
    },
    ScenarioDescriptor {
        name: Cow::Borrowed("p2-localization-mode-policy"),
        summary: Cow::Borrowed(
            "Checks localize mode, revision, status policy, and selector fallback.",
        ),
        kind: ScenarioKind::Headless,
        phase: phoxal::runtime::runtime::Phase::P2,
        timeout_secs: 60,
        category: Cow::Borrowed("localization"),
        tier: 1,
    },
    ScenarioDescriptor {
        name: Cow::Borrowed("p2-localization-rgbd-inertial-orb-slam3"),
        summary: Cow::Borrowed("Validates ORB-SLAM3 RGB-D inertial localization in Webots."),
        kind: ScenarioKind::Webots {
            world: Cow::Borrowed("LocalizationArena"),
        },
        phase: phoxal::runtime::runtime::Phase::P2,
        timeout_secs: 240,
        category: Cow::Borrowed("localization"),
        tier: 2,
    },
    ScenarioDescriptor {
        name: Cow::Borrowed("p2-localization-gnss-anchored"),
        summary: Cow::Borrowed("Validates GNSS-anchored localization against simulator truth."),
        kind: ScenarioKind::Webots {
            world: Cow::Borrowed("ArenaWorld"),
        },
        phase: phoxal::runtime::runtime::Phase::P2,
        timeout_secs: 120,
        category: Cow::Borrowed("localization"),
        tier: 2,
    },
    ScenarioDescriptor {
        name: Cow::Borrowed("p2-revision-loop-closure"),
        summary: Cow::Borrowed("Checks localization loop-closure revision convergence."),
        kind: ScenarioKind::Webots {
            world: Cow::Borrowed("ArenaWorld"),
        },
        phase: phoxal::runtime::runtime::Phase::P2,
        timeout_secs: 120,
        category: Cow::Borrowed("revision-convergence"),
        tier: 2,
    },
];

pub async fn run(name: &str, common: &RobotRuntimeArgs) -> Result<()> {
    match name {
        "p2-localization-backend-conformance" => p2_localization_backend_conformance(),
        "p2-localization-mode-policy" => p2_localization_mode_policy(common),
        "p2-localization-rgbd-inertial-orb-slam3" => {
            let ctx = webots_context(common).await?;
            assert_p2_localization_orb_slam3(&ctx, deadline_for(name)?).await
        }
        "p2-localization-gnss-anchored" => {
            let ctx = webots_context(common).await?;
            assert_p2_localization_gnss_anchored(&ctx, deadline_for(name)?).await
        }
        "p2-revision-loop-closure" => {
            let ctx = webots_context(common).await?;
            assert_p2_revision_loop_closure(&ctx, deadline_for(name)?).await
        }
        _ => anyhow::bail!("localize has no scenario '{name}'"),
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

fn p2_localization_backend_conformance() -> Result<()> {
    crate::conformance::run_localization_backend_conformance()
}

fn p2_localization_mode_policy(common: &RobotRuntimeArgs) -> Result<()> {
    let modes = [
        LocalizationMode::Initializing,
        LocalizationMode::DeadReckoning,
        LocalizationMode::Tracking,
        LocalizationMode::Relocalizing,
        LocalizationMode::Lost,
    ];
    ensure!(
        modes.len() == 5,
        "localization mode policy must cover every v1 mode variant"
    );

    let revision_causes = [
        LocalizationRevisionCause::SensorIntegration,
        LocalizationRevisionCause::LoopClosure,
        LocalizationRevisionCause::Relocalization,
        LocalizationRevisionCause::Reset,
        LocalizationRevisionCause::BackendRecovery,
    ];
    ensure!(
        revision_causes.len() == 5,
        "localization revision policy must cover every v1 cause variant"
    );

    let status_reasons = [
        LocalizationStatusReason::SensorMissing,
        LocalizationStatusReason::SensorStale,
        LocalizationStatusReason::BackendInitializing,
        LocalizationStatusReason::BackendError,
    ];
    ensure!(
        status_reasons.len() == 4,
        "localization status policy must cover every v1 reason variant"
    );

    let robot = fixture_robot(fixture_bundle_name(common)?)?;
    ensure!(
        matches!(
            phoxal::model::robot::v1::resolve_localize_backend(&robot.manifest, &robot.components),
            phoxal::model::robot::v1::ResolvedLocalizeBackend::OrbSlam3RgbdInertial { .. }
        ),
        "rgbd-imu-diff-drive fixture must expose the RGB-D + IMU localization sensor mix"
    );

    let backend = crate::selector::select_backend(&robot, None)?;
    ensure!(
        matches!(backend, BackendSelection::DeadReckoning),
        "ORB-SLAM3-eligible robot without vocabulary must fall back to dead-reckoning"
    );

    assert_schema::<LocalizationState>("runtime/localize/state", 1, "localization state")?;
    assert_schema::<LocalizationRevision>("runtime/localize/revision", 1, "localization revision")?;
    assert_schema::<Keyframe>("runtime/localize/keyframe", 1, "localize keyframe")?;
    assert_schema::<PoseGraphCorrection>("runtime/localize/correction", 1, "pose graph correction")
}

fn fixture_bundle_name(common: &RobotRuntimeArgs) -> Result<&str> {
    common
        .robot_config
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("robot config path must end with a fixture bundle name"))
}

#[derive(Debug, Clone, Copy)]
enum OrbExploreState {
    Wander { yaw_radps: f64, remaining_secs: f64 },
    Backup { spin_sign: f64, remaining_secs: f64 },
    Spin { yaw_radps: f64, remaining_secs: f64 },
}

#[derive(Debug, Clone)]
struct OrbExplorer {
    rng: u64,
    state: OrbExploreState,
    last_xy: Option<[f64; 2]>,
    window_progress_m: f64,
    window_secs: f64,
}

impl OrbExplorer {
    fn new() -> Self {
        let mut explorer = Self {
            rng: ORB_EXPLORER_SEED,
            state: OrbExploreState::Wander {
                yaw_radps: 0.0,
                remaining_secs: 0.0,
            },
            last_xy: None,
            window_progress_m: 0.0,
            window_secs: 0.0,
        };
        explorer.state = explorer.fresh_wander();
        explorer
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn next_range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_unit()
    }

    fn next_sign(&mut self) -> f64 {
        if self.next_u64() & 1 == 0 { -1.0 } else { 1.0 }
    }

    fn fresh_wander(&mut self) -> OrbExploreState {
        OrbExploreState::Wander {
            yaw_radps: self.next_range(-ORB_WANDER_TURN_MAX_RADPS, ORB_WANDER_TURN_MAX_RADPS),
            remaining_secs: ORB_WANDER_LEG_SECS,
        }
    }

    fn fresh_spin(&mut self, sign: f64) -> OrbExploreState {
        OrbExploreState::Spin {
            yaw_radps: sign * ORB_ESCAPE_SPIN_RADPS,
            remaining_secs: self.next_range(ORB_ESCAPE_SPIN_MIN_SECS, ORB_ESCAPE_SPIN_MAX_SECS),
        }
    }

    fn reset_stuck_window(&mut self) {
        self.window_progress_m = 0.0;
        self.window_secs = 0.0;
    }

    fn step(&mut self, truth_xy: [f64; 2]) -> ManualCommand {
        if let Some(last) = self.last_xy {
            self.window_progress_m += planar_distance(last, truth_xy);
        }
        self.last_xy = Some(truth_xy);
        self.window_secs += ORB_COMMAND_STEP_SECS;

        if matches!(self.state, OrbExploreState::Wander { .. })
            && self.window_secs >= ORB_STUCK_WINDOW_SECS
        {
            let stuck = self.window_progress_m < ORB_STUCK_MIN_PROGRESS_M;
            self.reset_stuck_window();
            if stuck {
                let spin_sign = self.next_sign();
                self.state = OrbExploreState::Backup {
                    spin_sign,
                    remaining_secs: ORB_ESCAPE_BACKUP_SECS,
                };
            }
        }

        let mut state = self.state;
        let command = match &mut state {
            OrbExploreState::Wander {
                yaw_radps,
                remaining_secs,
            } => {
                let command = ManualCommand {
                    linear_x_mps: ORB_WANDER_FORWARD_MPS,
                    angular_z_radps: *yaw_radps,
                };
                *remaining_secs -= ORB_COMMAND_STEP_SECS;
                command
            }
            OrbExploreState::Backup {
                spin_sign,
                remaining_secs,
            } => {
                let command = ManualCommand {
                    linear_x_mps: ORB_ESCAPE_BACKUP_MPS,
                    angular_z_radps: 0.0,
                };
                *remaining_secs -= ORB_COMMAND_STEP_SECS;
                if *remaining_secs <= 0.0 {
                    state = self.fresh_spin(*spin_sign);
                }
                command
            }
            OrbExploreState::Spin {
                yaw_radps,
                remaining_secs,
            } => {
                let command = ManualCommand {
                    linear_x_mps: 0.0,
                    angular_z_radps: *yaw_radps,
                };
                *remaining_secs -= ORB_COMMAND_STEP_SECS;
                command
            }
        };

        if matches!(
            state,
            OrbExploreState::Wander { remaining_secs, .. }
                | OrbExploreState::Spin { remaining_secs, .. } if remaining_secs <= 0.0
        ) {
            state = self.fresh_wander();
            self.reset_stuck_window();
        }
        self.state = state;
        command
    }
}

#[derive(Debug, Default)]
struct OrbTrackingProgress {
    saw_initializing: bool,
    saw_tracking_after_initializing: bool,
    tracking_before_initializing: bool,
    last_source: Option<LocalizationSource>,
    last_mode: Option<LocalizationMode>,
    last_revision_present: bool,
}

impl OrbTrackingProgress {
    fn new() -> Self {
        Self::default()
    }

    fn record(
        &mut self,
        source: LocalizationSource,
        mode: LocalizationMode,
        revision_present: bool,
    ) {
        self.last_source = Some(source);
        self.last_mode = Some(mode);
        self.last_revision_present = revision_present;
        match mode {
            LocalizationMode::Initializing => self.saw_initializing = true,
            LocalizationMode::Tracking => {
                if self.saw_initializing {
                    self.saw_tracking_after_initializing = true;
                } else {
                    self.tracking_before_initializing = true;
                }
            }
            LocalizationMode::DeadReckoning
            | LocalizationMode::Relocalizing
            | LocalizationMode::Lost => {}
            _ => {}
        }
    }
}

async fn assert_p2_localization_orb_slam3(ctx: &ScenarioContext, deadline: Instant) -> Result<()> {
    let mut progress = OrbTrackingProgress::new();
    let initial = ctx.latest_localization_state().await?;
    progress.record(
        initial.data.source,
        initial.data.mode,
        initial.data.revision.is_some(),
    );

    let mut explorer = OrbExplorer::new();
    let mut elapsed_secs = 0.0_f64;
    while elapsed_secs < ORB_WARMUP_SECS {
        ensure!(
            elapsed_secs < ORB_TRACKING_BUDGET_SECS,
            "ORB-SLAM3 warmup exhausted total {:.1}s tracking budget (last source {:?}, mode {:?}, revision_present {})",
            ORB_TRACKING_BUDGET_SECS,
            progress.last_source,
            progress.last_mode,
            progress.last_revision_present
        );
        drive_orb_explore_step(ctx, deadline, &mut explorer, "warmup").await?;
        elapsed_secs += ORB_COMMAND_STEP_SECS;
        let localize = ctx.latest_localization_state().await?;
        progress.record(
            localize.data.source,
            localize.data.mode,
            localize.data.revision.is_some(),
        );
    }

    ensure!(
        progress.saw_tracking_after_initializing || progress.tracking_before_initializing,
        "ORB-SLAM3 warmup did not reach Tracking within {:.1}s (last source {:?}, mode {:?}, revision_present {}, saw_initializing {})",
        ORB_WARMUP_SECS,
        progress.last_source,
        progress.last_mode,
        progress.last_revision_present,
        progress.saw_initializing
    );

    let mut stable_secs = 0.0_f64;
    while elapsed_secs < ORB_TRACKING_BUDGET_SECS {
        drive_orb_explore_step(ctx, deadline, &mut explorer, "stabilize").await?;
        elapsed_secs += ORB_COMMAND_STEP_SECS;
        let localize = ctx.latest_localization_state().await?;
        progress.record(
            localize.data.source,
            localize.data.mode,
            localize.data.revision.is_some(),
        );

        if localize.data.source == LocalizationSource::OrbSlam3RgbdInertial
            && localize.data.mode == LocalizationMode::Tracking
            && localize.data.revision.is_some()
        {
            stable_secs += ORB_COMMAND_STEP_SECS;
        } else {
            stable_secs = 0.0;
        }

        if stable_secs >= ORB_STABLE_WINDOW_SECS {
            let Some(est_0) = localize.data.pose else {
                return Err(anyhow!(
                    "ORB-SLAM3 stabilize reached Tracking for {:.1}s but has no pose estimate (source {:?}, mode {:?}, revision {:?})",
                    ORB_STABLE_WINDOW_SECS,
                    localize.data.source,
                    localize.data.mode,
                    localize.data.revision
                ));
            };
            let truth_0 = ctx.simulation_pose().await?.data;
            return assert_orb_tracking_trajectory(ctx, deadline, &mut explorer, truth_0, est_0)
                .await;
        }
    }

    Err(anyhow!(
        "ORB-SLAM3 RGB-D inertial localization did not stay Tracking for {:.1}s within {:.1}s budget (last source {:?}, mode {:?}, revision_present {}, saw_initializing {}, saw_tracking_after_initializing {}, stable_secs {:.2})",
        ORB_STABLE_WINDOW_SECS,
        ORB_TRACKING_BUDGET_SECS,
        progress.last_source,
        progress.last_mode,
        progress.last_revision_present,
        progress.saw_initializing,
        progress.saw_tracking_after_initializing,
        stable_secs
    ))
}

async fn assert_p2_localization_gnss_anchored(
    ctx: &ScenarioContext,
    deadline: Instant,
) -> Result<()> {
    loop {
        let localize = ctx.latest_localization_state().await?;
        if localize.data.source == LocalizationSource::GnssAnchored
            && localize.data.mode == LocalizationMode::Tracking
        {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "gnss-anchored localize not Tracking (source {:?}, mode {:?})",
            localize.data.source,
            localize.data.mode
        );
        ctx.advance_for_secs(0.5).await?;
    }

    loop {
        let localize = ctx.latest_localization_state().await?;
        let truth = ctx.simulation_pose().await?;
        if let Some(pose) = &localize.data.pose {
            let dx = pose.translation_m[0] - truth.data.translation_m[0];
            let dy = pose.translation_m[1] - truth.data.translation_m[1];
            if (dx * dx + dy * dy).sqrt() <= 0.50 {
                return Ok(());
            }
        }
        ensure!(
            Instant::now() < deadline,
            "gnss-anchored pose did not track ground truth"
        );
        ctx.advance_for_secs(0.5).await?;
    }
}

async fn assert_p2_revision_loop_closure(ctx: &ScenarioContext, deadline: Instant) -> Result<()> {
    wait_until_tracking(ctx, deadline).await?;

    loop {
        let summary: phoxal::bus::pubsub::Stamped<MapSummary> = ctx.latest_map_summary().await?;
        if summary.data.built_from_localize_revision.is_some() {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "map never linked to an initial localize revision"
        );
        ctx.advance_for_secs(0.5).await?;
    }

    loop {
        let summary = ctx.latest_map_summary().await?;
        if let Some(revision) = summary.data.built_from_localize_revision
            && revision.sequence > 0
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "map did not converge to the loop-closure revision (built_from {:?})",
            ctx.latest_map_summary()
                .await?
                .data
                .built_from_localize_revision
        );
        ctx.advance_for_secs(0.5).await?;
    }
}

async fn assert_orb_tracking_trajectory(
    ctx: &ScenarioContext,
    deadline: Instant,
    explorer: &mut OrbExplorer,
    truth_0: SimulatorPose,
    est_0: PoseEstimate,
) -> Result<()> {
    drive_orb_explore_segment(ctx, deadline, explorer, ORB_MEASURE_SECS, "measure").await?;

    let truth_end = ctx.simulation_pose().await?.data;
    let localize_end = ctx.latest_localization_state().await?;
    ensure!(
        localize_end.data.source == LocalizationSource::OrbSlam3RgbdInertial,
        "ORB-SLAM3 trajectory ended with wrong source {:?}",
        localize_end.data.source
    );
    ensure!(
        localize_end.data.revision.is_some(),
        "ORB-SLAM3 trajectory ended without localization revision"
    );
    let Some(est_end) = localize_end.data.pose else {
        return Err(anyhow!("ORB-SLAM3 trajectory ended without pose estimate"));
    };

    let truth_displacement_m =
        displacement_magnitude(&truth_0.translation_m, &truth_end.translation_m);
    let est_displacement_m = displacement_magnitude(&est_0.translation_m, &est_end.translation_m);
    let truth_rotation_rad = quat_geodesic_angle(truth_0.rotation_xyzw, truth_end.rotation_xyzw);
    let est_rotation_rad = quat_geodesic_angle(est_0.rotation_xyzw, est_end.rotation_xyzw);
    let displacement_error_m = (est_displacement_m - truth_displacement_m).abs();
    let rotation_error_rad = (est_rotation_rad - truth_rotation_rad).abs();

    ensure!(
        truth_displacement_m >= ORB_MIN_MEASURE_TRAVEL_M
            || truth_rotation_rad >= ORB_MIN_MEASURE_ROT_RAD,
        "ORB-SLAM3 measure window exercised too little motion to validate tracking (truth displacement {truth_displacement_m:.3}m, truth rotation {truth_rotation_rad:.3}rad)"
    );
    ensure!(
        displacement_error_m <= ORB_TRANSLATION_TOL_M,
        "ORB-SLAM3 measure displacement-magnitude error {displacement_error_m:.3}m exceeds {ORB_TRANSLATION_TOL_M:.3}m (truth {truth_displacement_m:.3}m, est {est_displacement_m:.3}m, source {:?}, mode {:?})",
        localize_end.data.source,
        localize_end.data.mode
    );
    ensure!(
        rotation_error_rad <= ORB_YAW_TOL_RAD,
        "ORB-SLAM3 measure rotation-angle error {rotation_error_rad:.3}rad exceeds {ORB_YAW_TOL_RAD:.3}rad (truth {truth_rotation_rad:.3}rad, est {est_rotation_rad:.3}rad, source {:?}, mode {:?})",
        localize_end.data.source,
        localize_end.data.mode
    );
    Ok(())
}

async fn drive_orb_explore_step(
    ctx: &ScenarioContext,
    deadline: Instant,
    explorer: &mut OrbExplorer,
    phase: &'static str,
) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "ORB-SLAM3 {phase} exceeded validate scenario wallclock deadline"
    );
    let truth = ctx.simulation_pose().await?.data;
    let truth_xy = [truth.translation_m[0], truth.translation_m[1]];
    let command = explorer.step(truth_xy);
    publish_and_advance(ctx, command, ORB_COMMAND_STEP_SECS).await
}

async fn drive_orb_explore_segment(
    ctx: &ScenarioContext,
    deadline: Instant,
    explorer: &mut OrbExplorer,
    duration_secs: f64,
    phase: &'static str,
) -> Result<()> {
    let mut elapsed_secs = 0.0_f64;
    let mut non_tracking_secs = 0.0_f64;

    while elapsed_secs < duration_secs {
        ensure!(
            Instant::now() < deadline,
            "ORB-SLAM3 {phase} exceeded validate scenario wallclock deadline"
        );
        let truth = ctx.simulation_pose().await?.data;
        let truth_xy = [truth.translation_m[0], truth.translation_m[1]];
        let command = explorer.step(truth_xy);
        publish_and_advance(ctx, command, ORB_COMMAND_STEP_SECS).await?;
        elapsed_secs += ORB_COMMAND_STEP_SECS;

        let localize = ctx.latest_localization_state().await?;
        ensure!(
            localize.data.source == LocalizationSource::OrbSlam3RgbdInertial,
            "ORB-SLAM3 {phase} switched source to {:?} while in mode {:?}",
            localize.data.source,
            localize.data.mode
        );
        ensure!(
            localize.data.revision.is_some(),
            "ORB-SLAM3 {phase} lost localization revision (source {:?}, mode {:?})",
            localize.data.source,
            localize.data.mode
        );
        match localize.data.mode {
            LocalizationMode::Tracking => {
                non_tracking_secs = 0.0;
            }
            LocalizationMode::Lost | LocalizationMode::Relocalizing => {
                non_tracking_secs += ORB_COMMAND_STEP_SECS;
                ensure!(
                    non_tracking_secs <= ORB_LOST_GRACE_SECS,
                    "ORB-SLAM3 {phase} stayed outside Tracking for {:.2}s (mode {:?}, revision {:?})",
                    non_tracking_secs,
                    localize.data.mode,
                    localize.data.revision
                );
            }
            mode => {
                return Err(anyhow!(
                    "ORB-SLAM3 {phase} reset or left Tracking/Relocalizing during measurement (mode {mode:?}, revision {:?})",
                    localize.data.revision
                ));
            }
        }
    }

    Ok(())
}

fn planar_distance(lhs_xy_m: [f64; 2], rhs_xy_m: [f64; 2]) -> f64 {
    let dx = lhs_xy_m[0] - rhs_xy_m[0];
    let dy = lhs_xy_m[1] - rhs_xy_m[1];
    (dx * dx + dy * dy).sqrt()
}

fn displacement_magnitude(start_m: &[f64; 3], end_m: &[f64; 3]) -> f64 {
    let dx = end_m[0] - start_m[0];
    let dy = end_m[1] - start_m[1];
    let dz = end_m[2] - start_m[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn quat_geodesic_angle(a: [f64; 4], b: [f64; 4]) -> f64 {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    2.0 * dot.abs().min(1.0).acos()
}
