use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::api::frame::v1::{FrameId, FrameLookupResponse, FrameTransform, Source};
use crate::api::localize::v1::{
    AffectedKeyframeSummary, Keyframe, KeyframeId, LocalizationRevision, LocalizationRevisionCause,
    LocalizationRevisionId, PoseEstimate as LocalizePoseEstimate, Region,
};
use crate::api::odometry::v1::{
    OdometryEstimate, PoseEstimate, Status, StatusMode, VelocityEstimate,
};
use crate::api::presence::v1::{Heartbeat, Readiness, RuntimeId, RuntimeReadiness, Summary};
use crate::api::simulation::pose::v1::Pose;
use crate::bus::topic::Topic;
use crate::model::v1;
use anyhow::{Context, Result, bail, ensure};
use nalgebra::{Isometry3, Quaternion, Translation3, UnitQuaternion};

const TRACK_WIDTH_M: f64 = 0.40;

pub fn assert_topic_schema<Kind>(topic: Topic<Kind>, schema_name: &str, label: &str) -> Result<()> {
    ensure!(
        topic.schema() == schema_name,
        "{label} schema name drifted: expected {schema_name}, got {}",
        topic.schema()
    );
    Ok(())
}

pub fn assert_close(name: &str, actual: f64, expected: f64, tolerance: f64) -> Result<()> {
    let delta = (actual - expected).abs();
    if delta <= tolerance {
        Ok(())
    } else {
        bail!(
            "{name} {actual:.6} differs from expected {expected:.6} by {delta:.6}, tolerance {tolerance:.6}"
        )
    }
}

pub fn heartbeat(runtime_id: &str, readiness: Readiness) -> Heartbeat {
    Heartbeat {
        runtime_id: RuntimeId::new(runtime_id),
        readiness,
    }
}

pub fn readiness_summary(mut heartbeats: Vec<Heartbeat>) -> Summary {
    heartbeats.sort_by(|left, right| left.runtime_id.0.cmp(&right.runtime_id.0));
    Summary {
        autonomy_ready: false,
        runtimes: heartbeats
            .into_iter()
            .map(|heartbeat| RuntimeReadiness {
                runtime_id: heartbeat.runtime_id,
                readiness: heartbeat.readiness,
            })
            .collect(),
    }
}

pub fn assert_ready_summary(summary: &Summary, expected_runtime_ids: &[&str]) -> Result<()> {
    let actual = summary
        .runtimes
        .iter()
        .map(|runtime| runtime.runtime_id.0.as_str())
        .collect::<BTreeSet<_>>();
    let expected = expected_runtime_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    ensure!(
        actual == expected,
        "presence summary runtimes differ: actual={actual:?}, expected={expected:?}"
    );
    ensure!(
        summary
            .runtimes
            .iter()
            .all(|runtime| runtime.readiness == Readiness::Ready),
        "presence summary includes a runtime that is not Ready"
    );
    Ok(())
}

pub fn readiness_for(summary: &Summary, runtime_id: &str) -> Option<Readiness> {
    summary
        .runtimes
        .iter()
        .find(|runtime| runtime.runtime_id.0 == runtime_id)
        .map(|runtime| runtime.readiness)
}

pub fn compose(
    parent_to_mid: &FrameTransform,
    mid_to_child: &FrameTransform,
    parent: &FrameId,
    child: &FrameId,
) -> FrameTransform {
    let composed = isometry(parent_to_mid) * isometry(mid_to_child);
    let rotation = composed.rotation.quaternion();
    FrameTransform {
        parent_frame_id: Some(parent.clone()),
        child_frame_id: child.clone(),
        translation_m: [
            composed.translation.vector.x,
            composed.translation.vector.y,
            composed.translation.vector.z,
        ],
        rotation_xyzw: [rotation.i, rotation.j, rotation.k, rotation.w],
        source: Source::Lookup,
    }
}

pub fn isometry(transform: &FrameTransform) -> Isometry3<f64> {
    Isometry3::from_parts(
        Translation3::new(
            transform.translation_m[0],
            transform.translation_m[1],
            transform.translation_m[2],
        ),
        UnitQuaternion::from_quaternion(Quaternion::new(
            transform.rotation_xyzw[3],
            transform.rotation_xyzw[0],
            transform.rotation_xyzw[1],
            transform.rotation_xyzw[2],
        )),
    )
}

pub fn ok_transform(response: FrameLookupResponse) -> Result<FrameTransform> {
    match response {
        FrameLookupResponse::Ok { transform, .. } => Ok(transform),
        FrameLookupResponse::UnknownFrame { frame_id } => {
            bail!("lookup reported unknown frame {frame_id}")
        }
        FrameLookupResponse::DisconnectedTree {
            parent_frame_id,
            child_frame_id,
        } => bail!("lookup reported disconnected tree {parent_frame_id}->{child_frame_id}"),
        FrameLookupResponse::ExtrapolationTooOld {
            oldest_available_ns,
        } => bail!("lookup reported data too old; oldest available {oldest_available_ns}"),
        FrameLookupResponse::ExtrapolationTooNew {
            newest_available_ns,
        } => bail!("lookup reported data too new; newest available {newest_available_ns}"),
        FrameLookupResponse::Busy => bail!("lookup reported busy"),
    }
}

pub fn fixture_robot(fixture_bundle: &str) -> Result<v1::Robot> {
    let workspace_root = workspace_root()?;
    let bundle_path = fixture_bundle_path(&workspace_root, fixture_bundle);
    v1::Robot::read_from_dir(&bundle_path).context("failed to load scenario fixture robot")
}

/// Resolve the workspace root from a runtime crate manifest directory.
///
/// Runtime crates live at `runtime/<name>/`, so walking two parents up reaches
/// the workspace root. The debug assertion catches layout drift.
pub fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .context("CARGO_MANIFEST_DIR not set; run via a runtime cargo package")?,
    );
    let Some(workspace_root) = manifest_dir.parent().and_then(|path| path.parent()) else {
        bail!(
            "runtime CARGO_MANIFEST_DIR must live two levels below the workspace root: {}",
            manifest_dir.display()
        );
    };
    let workspace_toml = workspace_root.join("Cargo.toml");
    debug_assert!(
        std::fs::read_to_string(&workspace_toml)
            .map(|contents| contents.contains("[workspace]"))
            .unwrap_or(false),
        "{} must be the workspace Cargo.toml",
        workspace_toml.display()
    );
    Ok(workspace_root.to_path_buf())
}

pub fn fixture_bundle_path(workspace_root: &Path, fixture_bundle: &str) -> PathBuf {
    workspace_root
        .join("fixture")
        .join("robot")
        .join(fixture_bundle)
}

pub fn localization_revision(
    epoch: u64,
    sequence: u64,
    previous_revision_id: Option<LocalizationRevisionId>,
) -> LocalizationRevision {
    let keyframe_id = KeyframeId::new(format!("kf-{sequence}"));
    LocalizationRevision {
        revision_id: LocalizationRevisionId { epoch, sequence },
        previous_revision_id,
        cause: LocalizationRevisionCause::SensorIntegration,
        affected_keyframes: AffectedKeyframeSummary {
            keyframe_ids: vec![keyframe_id],
            region: Some(Region {
                frame_id: FrameId::new("map"),
                min_xyz_m: [-1.0, -1.0, 0.0],
                max_xyz_m: [1.0, 1.0, 1.0],
            }),
        },
        inline_correction_available: false,
        correction_fetch_required: false,
    }
}

pub fn keyframe(keyframe_id: &str, revision: LocalizationRevisionId) -> Keyframe {
    Keyframe {
        keyframe_id: KeyframeId::new(keyframe_id),
        revision,
        pose: LocalizePoseEstimate {
            frame_id: FrameId::new("map"),
            child_frame_id: FrameId::new("base_link"),
            translation_m: [1.0, 0.0, 0.0],
            rotation_xyzw: yaw_quaternion(0.0),
        },
        descriptors: Vec::new(),
    }
}

pub fn estimate_from_wheel_delta(left_delta_m: f64, right_delta_m: f64) -> OdometryEstimate {
    let delta_center_m = (left_delta_m + right_delta_m) / 2.0;
    let delta_yaw_rad = (right_delta_m - left_delta_m) / TRACK_WIDTH_M;
    let (x, y) = if delta_yaw_rad.abs() <= f64::EPSILON {
        (delta_center_m, 0.0)
    } else {
        let radius = delta_center_m / delta_yaw_rad;
        (
            radius * delta_yaw_rad.sin(),
            radius * (1.0 - delta_yaw_rad.cos()),
        )
    };

    OdometryEstimate {
        pose: PoseEstimate {
            frame_id: FrameId::new("odom"),
            child_frame_id: FrameId::new("base_footprint"),
            translation_m: [x, y, 0.0],
            rotation_xyzw: yaw_quaternion(delta_yaw_rad),
        },
        velocity: VelocityEstimate {
            frame_id: FrameId::new("base_footprint"),
            linear_mps: [delta_center_m, 0.0, 0.0],
            angular_radps: [0.0, 0.0, delta_yaw_rad],
        },
        covariance: Some(crate::api::odometry::v1::Covariance {
            values: vec![0.0; 36],
        }),
        status: Status {
            mode: StatusMode::Tracking,
            reasons: Vec::new(),
        },
    }
}

pub fn pose_from_estimate(estimate: &OdometryEstimate) -> Pose {
    Pose {
        frame_id: estimate.pose.frame_id.0.clone(),
        translation_m: estimate.pose.translation_m,
        rotation_xyzw: estimate.pose.rotation_xyzw,
    }
}

pub fn origin_pose() -> Pose {
    Pose {
        frame_id: "odom".to_string(),
        translation_m: [0.0, 0.0, 0.0],
        rotation_xyzw: yaw_quaternion(0.0),
    }
}

pub fn yaw_quaternion(yaw_rad: f64) -> [f64; 4] {
    [0.0, 0.0, (yaw_rad / 2.0).sin(), (yaw_rad / 2.0).cos()]
}

pub fn yaw_from_xyzw(rotation_xyzw: [f64; 4]) -> f64 {
    let [x, y, z, w] = rotation_xyzw;
    let siny_cosp = 2.0 * (w * z + x * y);
    let cosy_cosp = 1.0 - 2.0 * (y * y + z * z);
    siny_cosp.atan2(cosy_cosp)
}
