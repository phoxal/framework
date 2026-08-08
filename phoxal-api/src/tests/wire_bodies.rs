//! The wire bodies themselves: that a body is its plain serde encoding, that it
//! survives the codec the bus uses, and that a foreign payload is rejected
//! rather than silently reinterpreted.

use super::{instant, round_trip};
use crate::{v0_1 as legacy, v0_2 as api};

#[test]
fn historic_v0_1_control_bodies_round_trip_without_shape_translation() {
    let target = legacy::drive::Target {
        linear_x_mps: 0.3,
        angular_z_radps: -0.2,
        curvature_limit_radpm: Some(0.4),
    };
    round_trip(&target);
    round_trip(&legacy::drive::State {
        target: target.clone(),
        limited_target: target.clone(),
        actuator_authority: legacy::drive::ActuatorAuthority::Active,
        stop_reason: None,
    });

    let motion_target = legacy::motion::Target {
        linear_x_mps: 0.1,
        angular_z_radps: 0.2,
        curvature_limit_radpm: None,
    };
    round_trip(&motion_target);
    round_trip(&legacy::motion::State {
        manual_observed_age_ns: Some(10),
        autonomous_candidate_age_ns: None,
        safety_constraints_age_ns: Some(5),
        selected_source: Some(legacy::motion::Source::Manual),
        final_target: motion_target,
        zero_reason: None,
        safety_runtime: legacy::motion::SafetyRuntime::Present,
        component_estop_blocked: false,
        active_safety_constraints: Vec::new(),
    });
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
        target: legacy::drive::Target,
        limited_target: legacy::drive::Target,
        actuator_authority: &'static str,
        stop_reason: Option<legacy::drive::StopReason>,
    }

    let target = legacy::drive::Target::stopped();
    let bytes = rmp_serde::to_vec_named(&OldState {
        target,
        limited_target: legacy::drive::Target::stopped(),
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
        source: crate::VideoSourceRef::parse("front_camera.rgb").unwrap(),
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
    let body = api::perception::Detections {
        source: crate::SourceRef::parse("front_camera.rgb").unwrap(),
        captured_at: phoxal_bus::TimeWindow::exact(instant(42)),
        detections: Vec::new(),
    };
    let json = serde_json::to_value(&body).unwrap();
    assert_eq!(json["source"], "front_camera.rgb");
    assert_eq!(
        json["captured_at"],
        serde_json::to_value(body.captured_at).unwrap()
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
        request_id: api::navigation::RequestId {
            value: "nav-1".to_string(),
        },
        outcome: api::navigation::Outcome::Failed(api::navigation::FailureReason::Blocked),
    };
    assert_eq!(
        serde_json::to_value(&navigation).unwrap(),
        serde_json::json!({
            "request_id": {"value": "nav-1"},
            "outcome": {"Failed": "blocked"}
        })
    );
    round_trip(&navigation);

    let constraint = api::safety::Constraint {
        reason: api::safety::ConstraintReason::ObstacleProximity,
        source: api::safety::ConstraintSource {
            kind: api::safety::ConstraintSourceKind::Range,
            participant_id: "safety".to_string(),
            component_id: Some("front".to_string()),
            capability_id: Some("range".to_string()),
        },
        stop: true,
        max_linear_speed_mps: None,
        max_angular_speed_radps: None,
        observed_value: Some(0.1),
        valid_from: instant(100),
        expires_at: instant(400),
    };
    let safety = api::safety::MotionConstraints {
        sequence: 3,
        stop: true,
        max_linear_speed_mps: None,
        max_angular_speed_radps: None,
        constraints: vec![constraint],
        expires_at: instant(400),
    };
    let safety_json = serde_json::to_value(&safety).unwrap();
    assert_eq!(
        safety_json["constraints"][0]["reason"],
        "obstacle_proximity"
    );
    assert_eq!(safety_json["constraints"][0]["source"]["kind"], "range");
    round_trip(&safety);
}

#[test]
fn navigation_and_safety_reject_malformed_payloads() {
    let wrong = rmp_serde::to_vec_named(&api::motion::ManualCommand {
        linear_x_mps: 0.1,
        angular_z_radps: 0.2,
    })
    .unwrap();
    assert!(rmp_serde::from_slice::<api::navigation::Request>(&wrong).is_err());
    assert!(rmp_serde::from_slice::<api::safety::MotionConstraints>(&wrong).is_err());
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
    round_trip(&api::power::State {
        status: api::power::Status::Idle,
        detail: None,
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
        });
    }
    round_trip(&api::safety::MotionConstraints {
        sequence: 1,
        stop: true,
        max_linear_speed_mps: Some(0.0),
        max_angular_speed_radps: Some(0.0),
        constraints: vec![api::safety::Constraint {
            reason: api::safety::ConstraintReason::ObstacleProximity,
            source: api::safety::ConstraintSource {
                kind: api::safety::ConstraintSourceKind::Range,
                participant_id: "front-range".to_string(),
                component_id: Some("front-range".to_string()),
                capability_id: Some("range".to_string()),
            },
            stop: true,
            max_linear_speed_mps: Some(0.0),
            max_angular_speed_radps: Some(0.0),
            observed_value: Some(0.1),
            valid_from: instant(10),
            expires_at: instant(310),
        }],
        expires_at: instant(310),
    });
    round_trip(&api::logs::Event {
        seq: 7,
        time: api::logs::Timestamp {
            unix_seconds: 1_800_000_000,
            nanos: 123,
        },
        level: api::logs::Level::Info,
        target: "phoxal.runtime".to_string(),
        message: "runtime ready".to_string(),
        fields: [(
            "participant".to_string(),
            api::logs::LogValue::String("drive".to_string()),
        )]
        .into_iter()
        .collect(),
        dropped: 2,
        truncated: 3,
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
        api::navigation::RequestId {
            value: "request-1".to_string(),
        },
    ));
    round_trip(&legacy::perception::Detections {
        detections: vec![legacy::perception::Detection {
            class_id: "crate".to_string(),
            confidence: 0.8,
            position_m: [1.0, 2.0, 3.0],
            frame_id: "camera_link".to_string(),
            track_id: Some(6),
        }],
        stamp: Some(instant(7)),
    });
    round_trip(&legacy::perception::State {
        healthy: true,
        detector: "legacy-detector".to_string(),
    });
    round_trip(&api::perception::Detections {
        source: crate::SourceRef::parse("front_camera.rgb").unwrap(),
        captured_at: phoxal_bus::TimeWindow::exact(instant(7)),
        detections: vec![api::perception::Detection {
            class_id: "crate".to_string(),
            confidence: 0.8,
            position_m: [1.0, 2.0, 3.0],
            frame_id: "camera_link".to_string(),
            track_id: Some(6),
        }],
    });
    round_trip(&api::perception::State::Healthy {
        detector: "deterministic-placeholder".to_string(),
    });
    round_trip(&api::perception::State::Unhealthy {
        detector: "deterministic-placeholder".to_string(),
        reason: api::perception::HealthReason::StaleCamera,
    });
    round_trip(&api::video::OpenOutcome::Unsupported);
    round_trip(&api::video::OpenOutcome::Unavailable);
}

/// `truncated` is `#[serde(default)]`, so a publisher that does not write the
/// field decodes as "nothing was truncated" rather than failing the whole
/// record.
#[test]
fn logs_event_defaults_truncation_for_publishers_without_the_field() {
    #[derive(serde::Serialize)]
    struct EventWithoutTruncation {
        seq: u64,
        time: api::logs::Timestamp,
        level: api::logs::Level,
        target: String,
        message: String,
        fields: std::collections::BTreeMap<String, api::logs::LogValue>,
        dropped: u32,
    }

    let bytes = rmp_serde::to_vec_named(&EventWithoutTruncation {
        seq: 1,
        time: api::logs::Timestamp {
            unix_seconds: 2,
            nanos: 3,
        },
        level: api::logs::Level::Info,
        target: "publisher".to_string(),
        message: "no truncation field".to_string(),
        fields: std::collections::BTreeMap::new(),
        dropped: 4,
    })
    .expect("encode event without the truncation field");
    let decoded: api::logs::Event =
        rmp_serde::from_slice(&bytes).expect("decode additive event field");
    assert_eq!(decoded.dropped, 4);
    assert_eq!(decoded.truncated, 0);
}

#[test]
fn retained_tool_contracts_round_trip_through_messagepack() {
    round_trip(&api::supervisor::log::SnapshotRequest {});
    let cursor = api::supervisor::Cursor {
        generation: "opaque-generation".to_string(),
        sequence: 9,
    };
    let record = api::supervisor::log::Record {
        sequence: 9,
        participant_id: "drive".to_string(),
        source_sequence: 41,
        time: api::supervisor::log::Timestamp {
            unix_seconds: 1_800_000_000,
            nanos: 123,
        },
        level: api::supervisor::log::Level::Info,
        target: "drive".to_string(),
        message: "target accepted".to_string(),
        fields: [(
            "speed".to_string(),
            api::supervisor::log::LogValue::F64(0.4),
        )]
        .into_iter()
        .collect(),
        dropped: 0,
        truncated: 0,
    };
    round_trip(&api::supervisor::log::Snapshot {
        cursor: cursor.clone(),
        ingest_dropped: 2,
        records: vec![record.clone()],
    });
    round_trip(&api::supervisor::log::Follow {
        cursor: cursor.clone(),
        ingest_dropped: 2,
        record,
    });

    let topic = api::supervisor::RuntimeTopic {
        topic: "v0.2/drive/state".to_string(),
        direction: api::supervisor::RuntimeDirection::Subscribe,
        buffer_kind: api::supervisor::RuntimeBufferKind::Latest,
        count: 42,
        rate_hz: 41.5,
        drops: 0,
        latest_overwrites: 41,
        bounded_evictions: 0,
        capacity: 1,
        current_depth: 1,
        high_water_depth: 1,
        decode_errors: 0,
        timeline_filtered: 0,
        overflowed_rows: 0,
    };
    let step = api::supervisor::RuntimeStep {
        target_period_ns: 20_000_000,
        completed: 49,
        errors: 1,
        mean_duration_ns: 2_000_000,
        max_duration_ns: 4_000_000,
        mean_lateness_ns: 10_000,
        max_lateness_ns: 100_000,
        missed_ticks: 0,
        overruns: 0,
    };
    round_trip(&api::supervisor::telemetry::Rollup {
        window_ns: 1_000_000_000,
        step: Some(step.clone()),
        topics: vec![topic.clone()],
        overflow: None,
    });
    round_trip(&api::supervisor::telemetry::SnapshotRequest {
        participant_id: Some("drive".to_string()),
        limit: 64,
        before_sequence: None,
    });
    let runtime_record = api::supervisor::telemetry::Record {
        sequence: 10,
        participant_id: "drive".to_string(),
        truncated: 0,
        window_ns: 1_000_000_000,
        step: Some(step),
        topics: vec![topic],
        overflow: None,
    };
    round_trip(&api::supervisor::telemetry::Snapshot {
        cursor: cursor.clone(),
        records: vec![runtime_record.clone()],
        capacity_evictions: 0,
        next_before_sequence: None,
    });
    round_trip(&api::supervisor::telemetry::Follow {
        cursor,
        record: runtime_record,
    });
}

#[test]
fn runtime_rollup_rejects_malformed_payloads() {
    let corrupt = [0xc1u8, 0xc1, 0xc1];
    assert!(rmp_serde::from_slice::<api::supervisor::telemetry::Rollup>(&corrupt).is_err());

    let wrong_shape = rmp_serde::to_vec_named(&api::supervisor::log::SnapshotRequest {}).unwrap();
    assert!(rmp_serde::from_slice::<api::supervisor::telemetry::Rollup>(&wrong_shape).is_err());
}

/// The clock body is the step counter and nothing else: its timeline and
/// instant ride in the envelope, stamped by the world authority.
#[test]
fn simulation_clock_body_carries_only_the_step_counter() {
    let clock = api::simulation::Clock { step: 100 };
    assert_eq!(
        serde_json::to_value(&clock).unwrap(),
        serde_json::json!({ "step": 100_u64 })
    );
    round_trip(&clock);
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
        encoding: api::component::camera::Encoding::Rgb8,
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
