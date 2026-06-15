use std::borrow::Cow;

use anyhow::{Result, bail, ensure};
use phoxal::api::frame::v1::{
    FrameId, FrameLookupRequest, FrameLookupResponse, FrameTransform, Source,
};
use phoxal::api::joint::v1::{JointId, JointState, Quantity};
use phoxal::runtime::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::helpers::{
    assert_close, compose, ok_transform, yaw_from_xyzw, yaw_quaternion,
};

pub const SCENARIOS: &[ScenarioDescriptor] = &[ScenarioDescriptor {
    name: Cow::Borrowed("frame-calibration"),
    summary: Cow::Borrowed("Checks frame transform composition and lookup response variants."),
    kind: ScenarioKind::Headless,
    phase: phoxal::runtime::Phase::P1,
    timeout_secs: 60,
    category: Cow::Borrowed("frame-calibration"),
    tier: 1,
}];

pub fn run(name: &str) -> Result<()> {
    match name {
        "frame-calibration" => frame_calibration(),
        _ => anyhow::bail!("frame has no scenario '{name}'"),
    }
}

fn frame_calibration() -> Result<()> {
    let timestamp_ns = 1_000_000_000;
    let joint_sample = JointState {
        value: 0.25,
        quantity: Quantity::AngleRad,
    };
    let joint_id = JointId::new("camera_tilt");
    let base = FrameId::new("base_link");
    let mast = FrameId::new("mast_link");
    let camera = FrameId::new("camera_depth_optical_frame");

    let base_to_mast = FrameTransform {
        parent_frame_id: Some(base.clone()),
        child_frame_id: mast.clone(),
        translation_m: [0.20, 0.0, 0.45],
        rotation_xyzw: yaw_quaternion(0.0),
        source: Source::Static,
    };
    let mast_to_camera = FrameTransform {
        parent_frame_id: Some(mast.clone()),
        child_frame_id: camera.clone(),
        translation_m: [0.0, 0.0, 0.10],
        rotation_xyzw: yaw_quaternion(joint_sample.value),
        source: Source::Joint { joint_id },
    };
    let composed = compose(&base_to_mast, &mast_to_camera, &base, &camera);

    let request = FrameLookupRequest {
        parent_frame_id: base.clone(),
        child_frame_id: camera.clone(),
        timestamp_ns,
    };
    let response = FrameLookupResponse::Ok {
        parent_frame_id: request.parent_frame_id.clone(),
        child_frame_id: request.child_frame_id.clone(),
        timestamp_ns: request.timestamp_ns,
        transform: composed,
    };
    let transform = ok_transform(response)?;
    assert_close("camera x", transform.translation_m[0], 0.20, 0.000_001)?;
    assert_close("camera z", transform.translation_m[2], 0.55, 0.000_001)?;
    assert_close(
        "camera yaw",
        yaw_from_xyzw(transform.rotation_xyzw),
        0.25,
        0.000_001,
    )?;

    let FrameLookupResponse::UnknownFrame { frame_id } = (FrameLookupResponse::UnknownFrame {
        frame_id: FrameId::new("missing_frame"),
    }) else {
        bail!("unknown frame lookup must be an explicit response variant");
    };
    ensure!(
        frame_id == FrameId::new("missing_frame"),
        "unknown frame response must carry the missing frame id"
    );

    let FrameLookupResponse::ExtrapolationTooOld {
        oldest_available_ns,
    } = (FrameLookupResponse::ExtrapolationTooOld {
        oldest_available_ns: timestamp_ns,
    })
    else {
        bail!("old frame lookup must be an explicit response variant");
    };
    ensure!(
        oldest_available_ns == timestamp_ns,
        "old frame response must carry the oldest available timestamp"
    );

    let FrameLookupResponse::ExtrapolationTooNew {
        newest_available_ns,
    } = (FrameLookupResponse::ExtrapolationTooNew {
        newest_available_ns: timestamp_ns,
    })
    else {
        bail!("new frame lookup must be an explicit response variant");
    };
    ensure!(
        newest_available_ns == timestamp_ns,
        "new frame response must carry the newest available timestamp"
    );

    Ok(())
}
