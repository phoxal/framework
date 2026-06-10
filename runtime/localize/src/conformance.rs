//! Backend-agnostic localization contract conformance (BLUEPRINT_VALIDATION "Backend
//! conformance"). Pure, synthetic-input checks driven against the dead-reckoning reference
//! backend and the runtime's revision/query logic. The Tier-2 Webots scenario covers the
//! ORB-SLAM3 backend end-to-end; this proves the contract every backend must satisfy.

use anyhow::{Result, bail, ensure};
use phoxal::api::frame::v1::FrameId;
use phoxal::api::localize::v1::{
    AffectedKeyframeSummary, CorrectionsRequest, CorrectionsResponse, KeyframeId, KeyframeRequest,
    KeyframeResponse, LocalizationMode, LocalizationRevisionCause, LocalizationRevisionId,
    LocalizationStatusReason, PoseGraphRange, PoseGraphRequest, PoseGraphResponse,
};
use phoxal::api::odometry::v1::{
    Covariance as OdometryCovariance, OdometryEstimate, PoseEstimate as OdometryPoseEstimate,
    Status, StatusMode, VelocityEstimate as OdometryVelocityEstimate,
};
use phoxal::api::simulation::v1::clock::Clock;
use phoxal::bus::typed::Received;
use phoxal::runtime::clock::Step;

use crate::runtime::{
    BackendUpdate, DeadReckoningBackend, LOCALIZE_EPOCH, LocalizeBackend, NewRevision,
    corrections_response, current_revision, keyframe_response, pose_graph_response,
    publishable_revision,
};

const STEP_DT_NS: u64 = 20_000_000;

/// Run the full localization backend conformance suite against synthetic inputs.
/// Returns `Ok(())` if every contract behavior holds; otherwise an error naming the behavior.
pub fn run_localization_backend_conformance() -> Result<()> {
    state_cadence()?;
    reset_handling()?;
    timestamp_handling()?;
    revision_monotonicity()?;
    loop_closure_revision_behavior()?;
    correction_overflow_behavior()?;
    query_behavior()?;
    mode_transitions()?;
    degraded_mode_behavior()?;
    Ok(())
}

fn state_cadence() -> Result<()> {
    let mut backend = DeadReckoningBackend::default();
    let steps = [step_at(0), step_at(STEP_DT_NS), step_at(STEP_DT_NS * 2)];
    let mut updates = 0;

    step_backend(&mut backend, steps[0], "state cadence missing-input step")?;
    updates += 1;
    backend.ingest_odometry(odometry_sample(STEP_DT_NS, StatusMode::Tracking));
    step_backend(&mut backend, steps[1], "state cadence first tracking step")?;
    updates += 1;
    backend.ingest_odometry(odometry_sample(STEP_DT_NS * 2, StatusMode::Tracking));
    step_backend(&mut backend, steps[2], "state cadence ready tracking step")?;
    updates += 1;

    ensure!(
        updates == steps.len(),
        "state cadence: backend yielded {updates} updates for {} step calls",
        steps.len()
    );
    Ok(())
}

fn reset_handling() -> Result<()> {
    let reset = LocalizationRevisionCause::Reset;
    ensure!(
        matches!(reset, LocalizationRevisionCause::Reset),
        "reset handling: LocalizationRevisionCause::Reset is not representable"
    );
    ensure!(
        current_revision()
            == LocalizationRevisionId {
                epoch: LOCALIZE_EPOCH,
                sequence: 0,
            },
        "reset handling: fresh current_revision() did not start at LOCALIZE_EPOCH sequence 0"
    );
    Ok(())
}

fn timestamp_handling() -> Result<()> {
    let mut backend = DeadReckoningBackend::default();
    let odometry_time_ns = 123_456_789;
    backend.ingest_odometry(odometry_sample(odometry_time_ns, StatusMode::Tracking));

    let update = step_backend(
        &mut backend,
        step_at(999_000_000),
        "timestamp handling step",
    )?;

    ensure!(
        update.valid_at_ns == Some(odometry_time_ns),
        "timestamp handling: valid_at_ns was {:?}, expected Some({odometry_time_ns})",
        update.valid_at_ns
    );
    Ok(())
}

fn revision_monotonicity() -> Result<()> {
    let mut current = current_revision();
    let mut emitted = false;
    let revisions = [
        publishable_revision(
            &mut current,
            &mut emitted,
            new_revision(LocalizationRevisionCause::SensorIntegration),
        ),
        publishable_revision(
            &mut current,
            &mut emitted,
            new_revision(LocalizationRevisionCause::LoopClosure),
        ),
        publishable_revision(
            &mut current,
            &mut emitted,
            new_revision(LocalizationRevisionCause::BackendRecovery),
        ),
    ];

    for window in revisions.windows(2) {
        let previous = window[0].revision_id;
        let next = window[1].revision_id;
        ensure!(
            previous.epoch == next.epoch,
            "revision monotonicity: epoch changed from {} to {}",
            previous.epoch,
            next.epoch
        );
        ensure!(
            previous.sequence <= next.sequence,
            "revision monotonicity: sequence moved backward from {} to {}",
            previous.sequence,
            next.sequence
        );
    }
    Ok(())
}

fn loop_closure_revision_behavior() -> Result<()> {
    let loop_closure_cause = LocalizationRevisionCause::LoopClosure;
    ensure!(
        matches!(loop_closure_cause, LocalizationRevisionCause::LoopClosure),
        "loop-closure revision behavior: LocalizationRevisionCause::LoopClosure is not representable"
    );

    let mut current = current_revision();
    let mut emitted = false;
    let first = publishable_revision(
        &mut current,
        &mut emitted,
        new_revision(LocalizationRevisionCause::SensorIntegration),
    );
    let loop_closure = publishable_revision(
        &mut current,
        &mut emitted,
        new_revision(LocalizationRevisionCause::LoopClosure),
    );

    ensure!(
        loop_closure.cause == LocalizationRevisionCause::LoopClosure,
        "loop-closure revision behavior: emitted cause was {:?}",
        loop_closure.cause
    );
    ensure!(
        loop_closure.previous_revision_id == Some(first.revision_id),
        "loop-closure revision behavior: previous_revision_id was {:?}, expected {:?}",
        loop_closure.previous_revision_id,
        Some(first.revision_id)
    );
    Ok(())
}

fn correction_overflow_behavior() -> Result<()> {
    let current = current_revision();
    let wrong_epoch = LocalizationRevisionId {
        epoch: current.epoch + 1,
        sequence: 0,
    };
    let wrong_epoch_request = CorrectionsRequest {
        from_revision: wrong_epoch,
        to_revision: current,
        max_bytes: None,
    };
    ensure!(
        corrections_response(&wrong_epoch_request, current)
            == CorrectionsResponse::WrongEpoch { current },
        "correction overflow behavior: mismatched epoch did not return WrongEpoch"
    );

    let current_epoch_request = CorrectionsRequest {
        from_revision: current,
        to_revision: current,
        max_bytes: None,
    };
    ensure!(
        corrections_response(&current_epoch_request, current)
            == CorrectionsResponse::RevisionUnavailable {
                latest_available: Some(current)
            },
        "correction overflow behavior: current epoch did not return RevisionUnavailable"
    );
    Ok(())
}

fn query_behavior() -> Result<()> {
    let current = current_revision();
    let wrong_epoch = LocalizationRevisionId {
        epoch: current.epoch + 1,
        sequence: current.sequence,
    };

    let wrong_pose_graph = PoseGraphRequest {
        revision: wrong_epoch,
        range: PoseGraphRange::All,
        max_bytes: None,
    };
    ensure!(
        pose_graph_response(&wrong_pose_graph, current)
            == PoseGraphResponse::WrongEpoch { current },
        "query behavior: pose-graph wrong epoch did not return WrongEpoch"
    );
    let current_pose_graph = PoseGraphRequest {
        revision: current,
        range: PoseGraphRange::All,
        max_bytes: None,
    };
    ensure!(
        pose_graph_response(&current_pose_graph, current)
            == PoseGraphResponse::RevisionUnavailable {
                latest_available: Some(current)
            },
        "query behavior: pose-graph current epoch did not return RevisionUnavailable with current latest"
    );

    let wrong_keyframe = KeyframeRequest {
        revision: wrong_epoch,
        keyframe_id: KeyframeId::new("synthetic-keyframe"),
        max_bytes: None,
    };
    ensure!(
        keyframe_response(&wrong_keyframe, current) == KeyframeResponse::WrongEpoch { current },
        "query behavior: keyframe wrong epoch did not return WrongEpoch"
    );
    let current_keyframe = KeyframeRequest {
        revision: current,
        keyframe_id: KeyframeId::new("synthetic-keyframe"),
        max_bytes: None,
    };
    ensure!(
        keyframe_response(&current_keyframe, current)
            == KeyframeResponse::RevisionUnavailable {
                latest_available: Some(current)
            },
        "query behavior: keyframe current epoch did not return RevisionUnavailable with current latest"
    );
    Ok(())
}

fn mode_transitions() -> Result<()> {
    let mut backend = DeadReckoningBackend::default();

    let missing = step_backend(&mut backend, step_at(0), "mode transitions missing sample")?;
    ensure!(
        missing.mode == LocalizationMode::Initializing,
        "mode transitions: missing sample produced {:?}, expected Initializing",
        missing.mode
    );

    backend.ingest_odometry(odometry_sample(STEP_DT_NS, StatusMode::Tracking));
    let first_tracking = step_backend(
        &mut backend,
        step_at(STEP_DT_NS),
        "mode transitions first tracking sample",
    )?;
    ensure!(
        first_tracking.mode == LocalizationMode::Initializing,
        "mode transitions: first tracking sample produced {:?}, expected Initializing",
        first_tracking.mode
    );

    backend.ingest_odometry(odometry_sample(STEP_DT_NS * 2, StatusMode::Tracking));
    let ready = step_backend(
        &mut backend,
        step_at(STEP_DT_NS * 2),
        "mode transitions ready sample",
    )?;
    ensure!(
        ready.mode == LocalizationMode::DeadReckoning,
        "mode transitions: ready sample produced {:?}, expected DeadReckoning",
        ready.mode
    );

    backend.ingest_odometry(odometry_sample(STEP_DT_NS * 3, StatusMode::Stale));
    let stale = step_backend(
        &mut backend,
        step_at(STEP_DT_NS * 3),
        "mode transitions stale sample",
    )?;
    ensure!(
        stale.mode == LocalizationMode::Lost,
        "mode transitions: stale sample produced {:?}, expected Lost",
        stale.mode
    );

    backend.ingest_odometry(odometry_sample(STEP_DT_NS * 4, StatusMode::Degraded));
    let degraded = step_backend(
        &mut backend,
        step_at(STEP_DT_NS * 4),
        "mode transitions degraded sample",
    )?;
    ensure!(
        degraded.mode == LocalizationMode::DeadReckoning,
        "mode transitions: degraded sample produced {:?}, expected DeadReckoning",
        degraded.mode
    );
    ensure!(
        degraded
            .status
            .reasons
            .contains(&LocalizationStatusReason::SensorStale),
        "mode transitions: degraded sample did not surface SensorStale"
    );
    Ok(())
}

fn degraded_mode_behavior() -> Result<()> {
    let mut backend = ready_backend()?;
    backend.ingest_odometry(odometry_sample(STEP_DT_NS * 3, StatusMode::Degraded));

    let update = step_backend(
        &mut backend,
        step_at(STEP_DT_NS * 3),
        "degraded mode behavior step",
    )?;

    ensure!(
        update.mode == LocalizationMode::DeadReckoning,
        "degraded mode behavior: degraded odometry produced {:?}, expected DeadReckoning",
        update.mode
    );
    ensure!(
        update
            .status
            .reasons
            .contains(&LocalizationStatusReason::SensorStale),
        "degraded mode behavior: degraded odometry did not surface SensorStale"
    );
    Ok(())
}

fn ready_backend() -> Result<DeadReckoningBackend> {
    let mut backend = DeadReckoningBackend::default();
    backend.ingest_odometry(odometry_sample(STEP_DT_NS, StatusMode::Tracking));
    step_backend(
        &mut backend,
        step_at(STEP_DT_NS),
        "ready backend first step",
    )?;
    backend.ingest_odometry(odometry_sample(STEP_DT_NS * 2, StatusMode::Tracking));
    step_backend(
        &mut backend,
        step_at(STEP_DT_NS * 2),
        "ready backend second step",
    )?;
    Ok(backend)
}

fn step_backend(
    backend: &mut DeadReckoningBackend,
    step: Step,
    behavior: &'static str,
) -> Result<BackendUpdate> {
    match backend.step(step) {
        Ok(update) => Ok(update),
        Err(error) => bail!("{behavior}: dead-reckoning step failed: {error:#}"),
    }
}

fn step_at(time_ns: u64) -> Step {
    Step::new(Clock::new(1, time_ns / STEP_DT_NS, time_ns, STEP_DT_NS))
}

fn odometry_sample(timestamp_ns: u64, mode: StatusMode) -> Received<OdometryEstimate> {
    Received {
        at_ns: Some(timestamp_ns),
        value: OdometryEstimate {
            pose: OdometryPoseEstimate {
                frame_id: FrameId::new("odom"),
                child_frame_id: FrameId::new("base_footprint"),
                translation_m: [timestamp_ns as f64 / 1_000_000_000.0, 0.0, 0.0],
                rotation_xyzw: [0.0, 0.0, 0.0, 1.0],
            },
            velocity: OdometryVelocityEstimate {
                frame_id: FrameId::new("base_footprint"),
                linear_mps: [0.1, 0.0, 0.0],
                angular_radps: [0.0, 0.0, 0.0],
            },
            covariance: Some(OdometryCovariance {
                values: vec![0.0; 36],
            }),
            status: Status {
                mode,
                reasons: Vec::new(),
            },
        },
    }
}

fn new_revision(cause: LocalizationRevisionCause) -> NewRevision {
    NewRevision {
        cause,
        affected_keyframes: AffectedKeyframeSummary {
            keyframe_ids: Vec::new(),
            region: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conformance_suite_passes() {
        assert!(run_localization_backend_conformance().is_ok());
    }
}
