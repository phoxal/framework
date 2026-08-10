//! The wire bodies themselves: that a body is its plain serde encoding, that it
//! survives the codec the bus uses, and that a foreign payload is rejected
//! rather than silently reinterpreted.

use super::{instant, round_trip};
use crate::v0_1 as api;
use serde::de::DeserializeOwned;

fn producer(value: u128) -> phoxal_bus::ProducerId {
    phoxal_bus::ProducerId::try_from((1_u128 << 124) | value).expect("canonical producer")
}

#[test]
fn body_serializes_as_plain_payload_without_version_tag() {
    let target = api::drive::Target::try_new(1.0, 0.5).unwrap();
    // MessagePack-as-JSON projection: the wire body is the plain struct, with no
    // `v`/`data` envelope around it.
    let json = serde_json::to_value(&target).unwrap();
    assert_eq!(json["linear_x_mps"], 1.0);
    assert!(
        json.get("v").is_none(),
        "wire body must not carry a version tag"
    );
    assert!(json.get("data").is_none());
}

#[test]
fn body_round_trips_through_messagepack() {
    let target = api::drive::Target::try_new(0.3, -0.2).unwrap();
    for state in [
        api::drive::State::Active {
            target: target.clone(),
            limited_target: target.clone(),
        },
        api::drive::State::Stopped {
            target: target.clone(),
            reason: api::drive::StopReason::TargetStale,
        },
    ] {
        let bytes = rmp_serde::to_vec_named(&state).unwrap();
        let decoded: api::drive::State = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(state, decoded);
    }
}

#[test]
fn target_rejects_non_finite_messagepack_scalars() {
    #[derive(serde::Serialize)]
    struct RawTarget {
        linear_x_mps: f32,
        angular_z_radps: f32,
    }

    for raw in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY]
        .into_iter()
        .flat_map(|value| {
            [
                RawTarget {
                    linear_x_mps: value,
                    angular_z_radps: 0.0,
                },
                RawTarget {
                    linear_x_mps: 0.0,
                    angular_z_radps: value,
                },
            ]
        })
    {
        let bytes = rmp_serde::to_vec_named(&raw).unwrap();
        assert!(rmp_serde::from_slice::<api::drive::Target>(&bytes).is_err());
    }

    let missing = serde_json::json!({"linear_x_mps": 0.0});
    let bytes = rmp_serde::to_vec_named(&missing).unwrap();
    assert!(rmp_serde::from_slice::<api::drive::Target>(&bytes).is_err());
}

#[test]
fn old_parallel_drive_state_is_not_a_valid_wire_state() {
    #[derive(serde::Serialize)]
    struct OldState {
        target: api::drive::Target,
        limited_target: api::drive::Target,
        actuator_authority: &'static str,
        stop_reason: Option<api::drive::StopReason>,
    }

    let target = api::drive::Target::stopped();
    let bytes = rmp_serde::to_vec_named(&OldState {
        target,
        limited_target: api::drive::Target::stopped(),
        actuator_authority: "Active",
        stop_reason: None,
    })
    .unwrap();
    assert!(rmp_serde::from_slice::<api::drive::State>(&bytes).is_err());

    let old_json = serde_json::json!({
        "target": {"linear_x_mps": 0.0, "angular_z_radps": 0.0},
        "limited_target": {"linear_x_mps": 0.0, "angular_z_radps": 0.0},
        "actuator_authority": "Active",
        "stop_reason": null,
    });
    assert!(serde_json::from_value::<api::drive::State>(old_json).is_err());
}

fn valid_grid_window() -> api::map::GridWindow {
    let bounds = api::map::Bounds {
        min_x_m: 0.0,
        min_y_m: 0.0,
        max_x_m: 0.1,
        max_y_m: 0.1,
    };
    api::map::GridWindow {
        frame_id: "map".to_string(),
        origin_pose: api::map::Pose {
            x_m: 0.0,
            y_m: 0.0,
            yaw_rad: 0.0,
        },
        cell_origin: api::map::Point { x_m: 0.0, y_m: 0.0 },
        resolution_m: 0.05,
        width: 2,
        height: 2,
        cells: vec![api::map::Occupancy::Free; 4],
        revision: 7,
        requested: bounds.clone(),
        covered: bounds,
    }
}

#[test]
fn v0_1_grid_window_is_self_describing_and_validated_on_deserialize() {
    let response = api::map::SubmapResponse::Window(valid_grid_window());
    round_trip(&response);

    let value = serde_json::to_value(response).unwrap();
    assert_eq!(value["Window"]["frame_id"], "map");
    assert_eq!(value["Window"]["revision"], 7);
    assert!(serde_json::from_value::<api::map::SubmapResponse>(value).is_ok());
}

#[test]
fn malformed_grid_shape_resolution_dimensions_and_cell_domain_are_rejected() {
    let base = serde_json::to_value(api::map::SubmapResponse::Window(valid_grid_window())).unwrap();
    for malformed in [
        serde_json::json!({
            "Window": {
                "frame_id": "map",
                "origin_pose": {"x_m": 0.0, "y_m": 0.0, "yaw_rad": 0.0},
                "cell_origin": {"x_m": 0.0, "y_m": 0.0},
                "resolution_m": 0.0,
                "width": 2,
                "height": 2,
                "cells": ["free", "free", "free", "free"],
                "revision": 1,
                "requested": {"min_x_m": 0.0, "min_y_m": 0.0, "max_x_m": 0.1, "max_y_m": 0.1},
                "covered": {"min_x_m": 0.0, "min_y_m": 0.0, "max_x_m": 0.1, "max_y_m": 0.1}
            }
        }),
        {
            let mut value = base.clone();
            value["Window"]["width"] = serde_json::json!(0);
            value
        },
        {
            let mut value = base.clone();
            value["Window"]["cells"] = serde_json::json!(["free"]);
            value
        },
        {
            let mut value = base.clone();
            value["Window"]["cells"] =
                serde_json::json!(["not-an-occupancy", "free", "free", "free"]);
            value
        },
        {
            let mut value = base.clone();
            value["Window"]["width"] = serde_json::json!(u32::MAX);
            value["Window"]["height"] = serde_json::json!(u32::MAX);
            value["Window"]["cells"] = serde_json::json!([]);
            value
        },
        {
            let mut value = base.clone();
            value["Window"]["covered"]["max_x_m"] = serde_json::json!(0.15);
            value
        },
        {
            let mut value = base.clone();
            value["Window"]["origin_pose"]["x_m"] = serde_json::json!("NaN");
            value
        },
    ] {
        assert!(
            serde_json::from_value::<api::map::SubmapResponse>(malformed).is_err(),
            "malformed grid payload must fail during deserialization"
        );
    }
}

#[test]
fn messagepack_grid_rejects_nonfinite_revisioned_fields() {
    let mut window = valid_grid_window();
    window.resolution_m = f32::NAN;
    let bytes = rmp_serde::to_vec_named(&api::map::SubmapResponse::Window(window)).unwrap();
    assert!(rmp_serde::from_slice::<api::map::SubmapResponse>(&bytes).is_err());
}

#[test]
fn old_target_extra_field_is_not_a_valid_wire_target() {
    let legacy_field = ["curvature", "limit_radpm"].join("_");
    let mut old_json = serde_json::Map::from_iter([
        ("linear_x_mps".to_string(), serde_json::json!(0.0)),
        ("angular_z_radps".to_string(), serde_json::json!(0.0)),
    ]);
    old_json.insert(legacy_field, serde_json::json!(0.4));
    assert!(
        serde_json::from_value::<api::drive::Target>(serde_json::Value::Object(old_json)).is_err()
    );
    assert!(
        serde_json::from_value::<api::drive::Target>(serde_json::json!({
            "linear_x_mps": 0.0,
        }))
        .is_err()
    );
}

#[test]
fn motor_commands_reject_nonfinite_values_during_deserialization() {
    for command in [
        api::component::motor::Command::Position(f32::NAN),
        api::component::motor::Command::Velocity(f32::INFINITY),
        api::component::motor::Command::Torque(f32::NEG_INFINITY),
    ] {
        let bytes = rmp_serde::to_vec_named(&command).expect("messagepack encodes the payload");
        assert!(
            rmp_serde::from_slice::<api::component::motor::Command>(&bytes).is_err(),
            "non-finite motor command must fail at the wire boundary"
        );
    }
}

#[test]
fn video_open_source_is_canonical_and_validated_during_deserialization() {
    let request = api::video::OpenRequest {
        source: api::perception::SourceRef::parse("front_camera.rgb").unwrap(),
        width_px: Some(640),
        height_px: Some(480),
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::json!({
            "source": "front_camera.rgb",
            "width_px": 640,
            "height_px": 480,
        })
    );
    round_trip(&request);

    for invalid in [
        "",
        "rgb",
        "front_camera_rgb",
        "front_camera.rgb.extra",
        " Front.rgb",
    ] {
        let json = serde_json::json!({"source": invalid});
        assert!(
            serde_json::from_value::<api::video::OpenRequest>(json).is_err(),
            "{invalid:?} must not deserialize as a video source"
        );
    }
}

#[test]
fn perception_source_capture_preserves_source_and_capture_window() {
    let body = api::perception::Detections::try_new(
        api::perception::SourceRef::parse("front_camera.rgb").unwrap(),
        phoxal_bus::TimeWindow::exact(instant(42)),
        Vec::new(),
    )
    .unwrap();
    let json = serde_json::to_value(&body).unwrap();
    assert_eq!(json["source"], "front_camera.rgb");
    assert_eq!(
        json["captured_at"],
        serde_json::to_value(body.captured_at()).unwrap()
    );
    round_trip(&body);

    for invalid in [
        "front_camera",
        "front_camera.rgb.extra",
        "FrontCamera.rgb",
        "front camera.rgb",
    ] {
        let mut malformed = json.clone();
        malformed["source"] = serde_json::Value::String(invalid.to_string());
        assert!(
            serde_json::from_value::<api::perception::Detections>(malformed).is_err(),
            "{invalid:?} must not deserialize as a perception source"
        );
    }
}

#[test]
fn current_perception_detection_rejects_nonfinite_and_wrong_shape_values() {
    #[derive(serde::Serialize)]
    struct RawDetection {
        class_id: &'static str,
        confidence: f32,
        position_m: [f64; 3],
        frame_id: &'static str,
        track_id: Option<u64>,
    }

    for raw in [
        RawDetection {
            class_id: "crate",
            confidence: f32::NAN,
            position_m: [1.0, 2.0, 3.0],
            frame_id: "camera_link",
            track_id: None,
        },
        RawDetection {
            class_id: "crate",
            confidence: 0.8,
            position_m: [f64::INFINITY, 2.0, 3.0],
            frame_id: "camera_link",
            track_id: None,
        },
    ] {
        let bytes = rmp_serde::to_vec_named(&raw).unwrap();
        assert!(
            rmp_serde::from_slice::<api::perception::Detection>(&bytes).is_err(),
            "non-finite perception detection must fail at the wire boundary"
        );
    }

    let wrong_shape = serde_json::json!({
        "class_id": "crate",
        "confidence": 0.8,
        "position_m": [1.0, 2.0],
        "frame_id": "camera_link",
        "track_id": null,
    });
    assert!(serde_json::from_value::<api::perception::Detection>(wrong_shape).is_err());
}

#[test]
fn navigation_and_safety_wire_shapes_are_golden() {
    let navigation = api::navigation::Result {
        operation_id: api::navigation::NavigationOperationId::new(producer(90), 4).unwrap(),
        request_id: api::navigation::RequestId::try_new("nav-1").unwrap(),
        outcome: api::navigation::Outcome::Failed(api::navigation::FailureReason::Blocked),
    };
    assert_eq!(
        serde_json::to_value(&navigation).unwrap(),
        serde_json::json!({
            "operation_id": {
                "producer": serde_json::to_value(producer(90)).unwrap(),
                "sequence": 4
            },
            "request_id": {"value": "nav-1"},
            "outcome": {"Failed": "blocked"}
        })
    );
    round_trip(&navigation);

    let constraint = api::safety::Constraint::Stopped {
        reason: api::safety::ConstraintReason::ObstacleProximity,
        source: api::safety::ConstraintSource {
            kind: api::safety::ConstraintSourceKind::Range,
            participant_id: "safety".to_string(),
            component_id: Some("front".to_string()),
            capability_id: Some("range".to_string()),
        },
        observed_value: Some(0.1),
        valid_from: instant(100),
        expires_at: instant(400),
    };
    let safety = api::safety::MotionConstraints {
        sequence: 3,
        permission: api::safety::MotionPermission::Stopped {
            reasons: vec![api::safety::ConstraintReason::ObstacleProximity],
        },
        constraints: vec![constraint],
        expires_at: instant(400),
    };
    let safety_json = serde_json::to_value(&safety).unwrap();
    assert_eq!(
        safety_json["constraints"][0]["Stopped"]["reason"],
        "obstacle_proximity"
    );
    assert_eq!(
        safety_json["constraints"][0]["Stopped"]["source"]["kind"],
        "range"
    );
    round_trip(&safety);
}

#[test]
fn navigation_and_safety_reject_malformed_payloads() {
    let wrong = rmp_serde::to_vec_named(&api::motion::ManualCommand {
        linear_x_mps: 0.1,
        angular_z_radps: 0.2,
    })
    .unwrap();
    assert!(rmp_serde::from_slice::<api::navigation::StartRequest>(&wrong).is_err());
    assert!(rmp_serde::from_slice::<api::safety::MotionConstraints>(&wrong).is_err());
}

#[test]
fn safety_permission_must_agree_with_constraints_on_json_and_messagepack_wire() {
    let constraint = api::safety::Constraint::Stopped {
        reason: api::safety::ConstraintReason::ObstacleProximity,
        source: api::safety::ConstraintSource {
            kind: api::safety::ConstraintSourceKind::Range,
            participant_id: "front-range".to_string(),
            component_id: Some("front".to_string()),
            capability_id: Some("range".to_string()),
        },
        observed_value: Some(0.1),
        valid_from: instant(100),
        expires_at: instant(400),
    };
    let safety = api::safety::MotionConstraints {
        sequence: 3,
        permission: api::safety::MotionPermission::Stopped {
            reasons: vec![api::safety::ConstraintReason::ObstacleProximity],
        },
        constraints: vec![constraint],
        expires_at: instant(400),
    };

    let mut divergent = serde_json::to_value(&safety).unwrap();
    divergent["permission"] = serde_json::to_value(api::safety::MotionPermission::Clear).unwrap();
    assert!(serde_json::from_value::<api::safety::MotionConstraints>(divergent.clone()).is_err());
    let bytes = rmp_serde::to_vec_named(&divergent).unwrap();
    assert!(rmp_serde::from_slice::<api::safety::MotionConstraints>(&bytes).is_err());

    let mut state = serde_json::to_value(api::safety::State {
        constraints: safety,
    })
    .unwrap();
    state["permission"] = serde_json::json!("Clear");
    assert!(serde_json::from_value::<api::safety::State>(state.clone()).is_err());
    let bytes = rmp_serde::to_vec_named(&state).unwrap();
    assert!(rmp_serde::from_slice::<api::safety::State>(&bytes).is_err());
}

#[test]
fn safety_nonfinite_limits_are_rejected_from_messagepack_wire() {
    let safety = api::safety::MotionConstraints {
        sequence: 3,
        permission: api::safety::MotionPermission::Limited {
            effective_linear_speed_mps: f32::NAN,
            effective_angular_speed_radps: 0.2,
            reasons: vec![api::safety::ConstraintReason::ObstacleProximity],
        },
        constraints: vec![api::safety::Constraint::Limited {
            reason: api::safety::ConstraintReason::ObstacleProximity,
            source: api::safety::ConstraintSource {
                kind: api::safety::ConstraintSourceKind::Range,
                participant_id: "front-range".to_string(),
                component_id: Some("front".to_string()),
                capability_id: Some("range".to_string()),
            },
            max_linear_speed_mps: f32::NAN,
            max_angular_speed_radps: 0.2,
            observed_value: Some(0.4),
            valid_from: instant(100),
            expires_at: instant(400),
        }],
        expires_at: instant(400),
    };
    let bytes = rmp_serde::to_vec_named(&safety).unwrap();
    assert!(rmp_serde::from_slice::<api::safety::MotionConstraints>(&bytes).is_err());
}

#[test]
fn domain_bodies_round_trip_through_messagepack() {
    round_trip(&api::joint::JointState {
        position_rad: 1.0,
        velocity_radps: 0.2,
        effort_nm: Some(0.3),
    });
    round_trip(&api::frame::Tree {
        transforms: vec![api::frame::FrameTransform {
            parent_frame_id: "map".to_string(),
            child_frame_id: "base_link".to_string(),
            translation_m: [1.0, 2.0, 0.0],
            rotation_quat_xyzw: [0.0, 0.0, 0.0, 1.0],
            stamp: Some(instant(10)),
        }],
    });
    for decision in [
        api::motion::Decision::Active {
            source: api::motion::Source::Manual,
            target: api::drive::Target::try_new(0.1, 0.2).unwrap(),
        },
        api::motion::Decision::Stopped {
            reason: api::motion::ZeroReason::SafetyProtectiveStop,
        },
    ] {
        round_trip(&api::motion::State {
            decision,
            manual_observed_age_ns: Some(10),
            autonomous_candidate_age_ns: None,
            safety_constraints_age_ns: Some(5),
            safety_runtime: api::motion::SafetyRuntime::Present,
            component_estop_blocked: false,
            active_safety_constraints: Vec::new(),
            safety_permission: api::safety::MotionPermission::Clear,
        });
    }
    round_trip(&api::safety::MotionConstraints {
        sequence: 1,
        permission: api::safety::MotionPermission::Stopped {
            reasons: vec![api::safety::ConstraintReason::ObstacleProximity],
        },
        constraints: vec![api::safety::Constraint::Stopped {
            reason: api::safety::ConstraintReason::ObstacleProximity,
            source: api::safety::ConstraintSource {
                kind: api::safety::ConstraintSourceKind::Range,
                participant_id: "front-range".to_string(),
                component_id: Some("front-range".to_string()),
                capability_id: Some("range".to_string()),
            },
            observed_value: Some(0.1),
            valid_from: instant(10),
            expires_at: instant(310),
        }],
        expires_at: instant(310),
    });
    round_trip(&api::navigation::Path {
        poses: vec![api::navigation::Pose {
            x_m: 1.0,
            y_m: 2.0,
            yaw_rad: None,
        }],
        map_revision: Some(3),
    });
    round_trip(&api::navigation::State::Running(
        api::navigation::NavigationOperationId::new(producer(91), 1).unwrap(),
    ));
    let detection =
        api::perception::Detection::try_new("crate", 0.8, [1.0, 2.0, 3.0], "camera_link").unwrap();
    let mut detection = detection;
    detection.set_track_id(Some(6));
    round_trip(
        &api::perception::Detections::try_new(
            api::perception::SourceRef::parse("front_camera.rgb").unwrap(),
            phoxal_bus::TimeWindow::exact(instant(7)),
            vec![detection],
        )
        .unwrap(),
    );
    round_trip(&api::perception::State::healthy("deterministic-placeholder").unwrap());
    round_trip(
        &api::perception::State::unhealthy(
            "deterministic-placeholder",
            api::perception::HealthReason::StaleCamera,
        )
        .unwrap(),
    );
    round_trip(&api::video::OpenOutcome::Unsupported);
    round_trip(&api::video::OpenOutcome::Unavailable);
}

#[test]
fn component_capability_bodies_round_trip_through_messagepack() {
    let imu = api::component::imu::Sample {
        orientation: Some([1.0, 0.0, 0.0, 0.0]),
        angular_velocity_radps: [0.1, 0.2, 0.3],
        linear_acceleration_mps2: [1.0, 2.0, 9.81],
        covariance: Some([0.0; 9]),
        noise_density: Some([0.01, 0.02, 0.03]),
        sensor_frame_id: Some("imu_link".to_string()),
        health: api::component::imu::SensorHealth::Degraded,
        bias: Some(api::component::imu::Bias {
            angular_velocity_radps: [0.001, 0.002, 0.003],
            linear_acceleration_mps2: [0.01, 0.02, 0.03],
        }),
    };
    let bytes = rmp_serde::to_vec_named(&imu).unwrap();
    let decoded: api::component::imu::Sample = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(imu, decoded);

    let frame = api::component::camera::Frame {
        width: 640,
        height: 480,
        encoding: api::component::camera::Encoding::Jpeg,
        intrinsics: Some(api::component::camera::Intrinsics {
            fx: 500.0,
            fy: 501.0,
            cx: 320.0,
            cy: 240.0,
        }),
        distortion: Some(api::component::camera::Distortion {
            model: "plumb_bob".to_string(),
            coefficients: vec![0.1, 0.2, 0.3],
        }),
        exposure: Some(api::component::camera::ExposureTiming {
            exposure_start_ns: Some(100),
            exposure_duration_ns: Some(200),
        }),
        calibration: Some(api::component::camera::CalibrationIdentity {
            id: "front".to_string(),
            version: "v0.1".to_string(),
        }),
        data: vec![1, 2, 3, 4],
    };
    let bytes = rmp_serde::to_vec_named(&frame).unwrap();
    let decoded: api::component::camera::Frame = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(frame, decoded);
}

fn rejects_json_and_messagepack<T: DeserializeOwned>(value: serde_json::Value) {
    assert!(serde_json::from_value::<T>(value.clone()).is_err());
    assert!(rmp_serde::from_slice::<T>(&rmp_serde::to_vec_named(&value).unwrap()).is_err());
}

#[test]
fn v0_1_sensor_wires_reject_malformed_json_and_messagepack() {
    rejects_json_and_messagepack::<api::component::camera::Frame>(serde_json::json!({
        "width": 0, "height": 1, "encoding": "jpeg", "intrinsics": null, "distortion": null,
        "exposure": null, "calibration": null, "data": []
    }));
    rejects_json_and_messagepack::<api::component::depth::Frame>(serde_json::json!({
        "samples_mm": [], "encoding": "u16_millimeters", "invalid_sample_policy": "zero_is_invalid",
        "width": 0, "height": 1, "intrinsics": null, "distortion": null, "exposure": null, "calibration": null
    }));
    rejects_json_and_messagepack::<api::component::range::Sample>(serde_json::json!({
        "distance_m": -1.0, "limits": null, "quality": null, "health": "nominal"
    }));
    rejects_json_and_messagepack::<api::component::battery::State>(serde_json::json!({
        "voltage_v": 12.0, "current_a": 0.0, "charge_ratio": 1.1
    }));
    rejects_json_and_messagepack::<api::component::gnss::Sample>(serde_json::json!({
        "latitude": 91.0, "longitude": 0.0, "altitude": 0.0, "position_covariance": [0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0]
    }));
    rejects_json_and_messagepack::<api::component::imu::Sample>(serde_json::json!({
        "orientation": [2.0,0.0,0.0,0.0], "angular_velocity_radps": [0.0,0.0,0.0], "linear_acceleration_mps2": [0.0,0.0,0.0],
        "covariance": null, "noise_density": null, "sensor_frame_id": null, "health": "nominal", "bias": null
    }));
    rejects_json_and_messagepack::<api::component::accelerometer::Sample>(
        serde_json::json!({"linear_acceleration": [0.0, 0.0]}),
    );
    rejects_json_and_messagepack::<api::component::gyroscope::Sample>(
        serde_json::json!({"angular_velocity": [0.0, 0.0]}),
    );
    rejects_json_and_messagepack::<api::component::magnetometer::Sample>(
        serde_json::json!({"magnetic_field": [0.0, 0.0]}),
    );
    rejects_json_and_messagepack::<api::component::encoder::Sample>(
        serde_json::json!({"position_rad": 0.0, "velocity_radps": "bad"}),
    );
    rejects_json_and_messagepack::<api::component::mmwave::Scan>(
        serde_json::json!({"detections": [{"position": [0.0,0.0,0.0], "velocity": [0.0,0.0,0.0], "snr": "bad"}]}),
    );
    rejects_json_and_messagepack::<api::component::lidar::Scan>(
        serde_json::json!({"kind": "ranges"}),
    );
}
