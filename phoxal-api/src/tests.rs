//! Golden tests for the generated API layer: the version-local body shape (plain
//! payload, no `{"v":…}` wrapper — D62), the `ContractBody` consts, the
//! `ApiVersion` id, and the topic keys produced by the api-local builders.

use crate::v0_1;
use crate::v0_1 as api;
use crate::{ApiVersion, ContractBody};
use phoxal_bus::{RobotInstant, TimelineId, TopicRole};

/// A same-timeline instant for round-trip fixtures.
fn instant(ticks: u64) -> RobotInstant {
    RobotInstant::new(TimelineId::from_raw(1).unwrap(), ticks)
}

#[test]
fn v0_1_is_the_single_train_selected_revision() {
    assert_eq!(<api::Api as ApiVersion>::ID, "v0.1");
    assert_eq!(<crate::latest::Api as ApiVersion>::ID, "v0.1");
}

#[test]
fn contract_body_topic_is_version_qualified() {
    // D1: the API version is folded into the wire key, so two participants using
    // the same version-qualified contract share a key that cannot collide with
    // any other version's contract of the same leaf name.
    assert_eq!(
        <api::drive::State as ContractBody>::TOPIC,
        "v0.1/drive/state"
    );
    assert_eq!(
        <api::drive::Target as ContractBody>::TOPIC,
        "v0.1/drive/target"
    );
    assert_eq!(
        <api::navigation::State as ContractBody>::TOPIC,
        "v0.1/navigation/state"
    );
    assert_eq!(
        <api::joint::JointState as ContractBody>::TOPIC,
        "v0.1/joint/{joint}/state"
    );
    assert_eq!(
        <api::video::stream::StreamState as ContractBody>::TOPIC,
        "v0.1/video/stream/{stream}/state"
    );
    assert_eq!(
        <api::localize::LocalizationState as ContractBody>::TOPIC,
        "v0.1/localize/state"
    );
    assert_eq!(
        <api::logs::Event as ContractBody>::TOPIC,
        "v0.1/logs/{participant_id}"
    );
    assert_eq!(
        <api::bus::uplink::State as ContractBody>::TOPIC,
        "v0.1/bus/uplink/state"
    );
    assert_eq!(
        <api::tool::log::SnapshotRequest as ContractBody>::TOPIC,
        "v0.1/tool/log/snapshot"
    );
    assert_eq!(
        <api::tool::log::Snapshot as ContractBody>::TOPIC,
        "v0.1/tool/log/snapshot"
    );
    assert_eq!(
        <api::tool::log::Follow as ContractBody>::TOPIC,
        "v0.1/tool/log/follow"
    );
    assert_eq!(
        <api::tool::bus::SnapshotRequest as ContractBody>::TOPIC,
        "v0.1/tool/bus/snapshot"
    );
    assert_eq!(
        <api::tool::bus::Snapshot as ContractBody>::TOPIC,
        "v0.1/tool/bus/snapshot"
    );
    assert_eq!(
        <api::tool::bus::Follow as ContractBody>::TOPIC,
        "v0.1/tool/bus/follow"
    );
    assert_eq!(
        <api::tool::runtime::Rollup as ContractBody>::TOPIC,
        "v0.1/tool/runtime/rollup"
    );
    assert_eq!(
        <api::tool::runtime::Snapshot as ContractBody>::TOPIC,
        "v0.1/tool/runtime/snapshot"
    );
    assert_eq!(
        <api::tool::runtime::Follow as ContractBody>::TOPIC,
        "v0.1/tool/runtime/follow"
    );
    assert_eq!(
        <api::tool::device::Sample as ContractBody>::TOPIC,
        "v0.1/tool/device/sample"
    );
    assert_eq!(
        <api::tool::device::SnapshotRequest as ContractBody>::TOPIC,
        "v0.1/tool/device/snapshot"
    );
    assert_eq!(
        <api::tool::device::Follow as ContractBody>::TOPIC,
        "v0.1/tool/device/follow"
    );
}

#[test]
fn folded_contracts_are_available_on_the_first_revision() {
    assert_eq!(<v0_1::Api as ApiVersion>::ID, "v0.1");

    assert_eq!(
        <v0_1::battery::State as ContractBody>::TOPIC,
        "v0.1/battery/state"
    );
    assert_eq!(
        v0_1::topic::new().battery().state().key(),
        "v0.1/battery/state"
    );
    assert_eq!(
        <v0_1::simulation::Clock as ContractBody>::TOPIC,
        "v0.1/simulation/clock"
    );
    assert_eq!(
        v0_1::topic::new().simulation().clock().key(),
        "v0.1/simulation/clock"
    );

    assert_eq!(
        <v0_1::joypad::Devices as ContractBody>::TOPIC,
        "v0.1/joypad/devices"
    );
    assert_eq!(
        v0_1::topic::new().joypad().devices().key(),
        "v0.1/joypad/devices"
    );
    assert_eq!(
        <v0_1::joypad::Select as ContractBody>::TOPIC,
        "v0.1/joypad/select"
    );
    assert_eq!(
        v0_1::topic::new().joypad().select().key(),
        "v0.1/joypad/select"
    );
    assert_eq!(
        <v0_1::joypad::SetEnabled as ContractBody>::TOPIC,
        "v0.1/joypad/set_enabled"
    );
    assert_eq!(
        v0_1::topic::new().joypad().set_enabled().key(),
        "v0.1/joypad/set_enabled"
    );
    assert_eq!(
        <v0_1::joypad::Rescan as ContractBody>::TOPIC,
        "v0.1/joypad/rescan"
    );
    assert_eq!(
        v0_1::topic::new().joypad().rescan().key(),
        "v0.1/joypad/rescan"
    );
}

#[test]
fn generated_contract_manifest_lists_contract_shapes() {
    let version = crate::API_CONTRACT_MANIFEST
        .iter()
        .find(|version| version.name == "v0.1")
        .expect("v0.1 should be in the generated manifest");

    let drive_state = version
        .contracts
        .iter()
        .find(|contract| contract.family == "v0.1::drive::State")
        .expect("drive::State should be in the generated manifest");
    assert_eq!(drive_state.topic, "v0.1/drive/state");
    let device_sample = version
        .contracts
        .iter()
        .find(|contract| contract.family == "v0.1::tool::device::Sample")
        .expect("tool::device::Sample should be in the generated manifest");
    assert_eq!(device_sample.topic, "v0.1/tool/device/sample");

    {
        let current = crate::API_CONTRACT_MANIFEST
            .iter()
            .find(|version| version.name == "v0.1")
            .expect("v0.1 should be in the generated manifest");

        let battery_state = current
            .contracts
            .iter()
            .find(|contract| contract.family == "v0.1::battery::State")
            .expect("battery::State should be in the v0.1 manifest entry");
        assert_eq!(battery_state.topic, "v0.1/battery/state");
        for family in [
            "v0.1::simulation::Clock",
            "v0.1::joypad::Devices",
            "v0.1::joypad::Select",
            "v0.1::joypad::SetEnabled",
            "v0.1::joypad::Rescan",
        ] {
            assert!(
                current
                    .contracts
                    .iter()
                    .any(|contract| contract.family == family),
                "{family} should be in the v0.1 manifest entry"
            );
        }
    }

    assert_eq!(
        crate::API_CONTRACT_MANIFEST.len(),
        1,
        "the train ships exactly one concrete revision"
    );
}

#[test]
fn generated_role_const_matches_each_topic_role() {
    // `ContractBody::ROLE` is generated per body by `phoxal_api_tree!`, and it
    // fixes both the side brand and the robot time a publisher may express.
    // Assert representative topics across every role so a generator bug that
    // emitted an all-`State` tree is caught by `cargo test`.

    // Command: a control input the owning service subscribes. It expresses no
    // robot time.
    assert_role::<api::drive::Target>(TopicRole::Command);
    assert_role::<api::power::Command>(TopicRole::Command);
    assert_role::<api::motion::ManualCommand>(TopicRole::Command);
    assert_role::<api::joypad::Select>(TopicRole::Command);
    assert_role::<api::joypad::SetEnabled>(TopicRole::Command);
    assert_role::<api::joypad::Rescan>(TopicRole::Command);

    // State: what the owning service publishes at a logical step.
    assert_role::<api::drive::State>(TopicRole::State);
    assert_role::<api::battery::State>(TopicRole::State);
    assert_role::<api::simulation::Clock>(TopicRole::State);
    assert_role::<api::component::emergency_stop::State>(TopicRole::State);

    // Measurement: a sensor observation carrying a capture stamp.
    assert_role::<api::component::imu::Sample>(TopicRole::Measurement);
    assert_role::<api::component::encoder::Sample>(TopicRole::Measurement);
    assert_role::<api::component::camera::Frame>(TopicRole::Measurement);
    assert_role::<api::component::depth::Frame>(TopicRole::Measurement);
    assert_role::<api::component::lidar::Scan>(TopicRole::Measurement);
    assert_role::<api::component::range::Sample>(TopicRole::Measurement);

    // Diagnostic: describes the participant or host, never the world, and so
    // expresses no robot time.
    assert_role::<api::tool::runtime::Rollup>(TopicRole::Diagnostic);
    assert_role::<api::tool::runtime::Follow>(TopicRole::Diagnostic);
    assert_role::<api::tool::device::Sample>(TopicRole::Diagnostic);
    assert_role::<api::tool::device::Follow>(TopicRole::Diagnostic);
    assert_role::<api::logs::Event>(TopicRole::Diagnostic);
    assert_role::<api::joypad::Devices>(TopicRole::Diagnostic);

    // Query: both the request and the response body of a request/response topic
    // carry the `Query` role.
    assert_role::<api::frame::LookupRequest>(TopicRole::Query);
    assert_role::<api::frame::LookupResponse>(TopicRole::Query);
    assert_role::<api::map::SubmapRequest>(TopicRole::Query);
    assert_role::<api::map::SubmapResponse>(TopicRole::Query);
    assert_role::<api::tool::runtime::SnapshotRequest>(TopicRole::Query);
    assert_role::<api::tool::device::Snapshot>(TopicRole::Query);
}

fn assert_role<B: ContractBody>(expected: TopicRole) {
    assert_eq!(B::ROLE, expected, "{} has the wrong role", B::NAME);
}

#[test]
fn body_serializes_as_plain_payload_without_version_tag() {
    let target = api::drive::Target {
        linear_x_mps: 1.0,
        angular_z_radps: 0.5,
        curvature_limit_radpm: None,
    };
    // MessagePack-as-JSON projection: the wire body is the plain struct — there is
    // no `v`/`data` envelope around it.
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
    let state = api::drive::State {
        target: api::drive::Target {
            linear_x_mps: 0.3,
            angular_z_radps: -0.2,
            curvature_limit_radpm: None,
        },
        limited_target: api::drive::Target {
            linear_x_mps: 0.3,
            angular_z_radps: -0.2,
            curvature_limit_radpm: None,
        },
        actuator_authority: api::drive::ActuatorAuthority::Active,
        stop_reason: None,
    };
    let bytes = rmp_serde::to_vec_named(&state).unwrap();
    let decoded: api::drive::State = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(state, decoded);
}

#[test]
fn behavior_navigation_and_safety_wire_shapes_are_golden() {
    let request = api::behavior::Request {
        request_id: api::behavior::RequestId {
            value: "req-7".to_string(),
        },
        behavior_id: "navigation.return_to_dock".to_string(),
        args: std::collections::BTreeMap::new(),
        priority: 9,
        conflict_policy: api::behavior::ConflictPolicy::Queue,
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::json!({
            "request_id": {"value": "req-7"},
            "behavior_id": "navigation.return_to_dock",
            "args": {},
            "priority": 9,
            "conflict_policy": "queue"
        })
    );
    round_trip(&request);

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
fn behavior_navigation_and_safety_reject_malformed_payloads() {
    let corrupt = [0xc1u8, 0xc1, 0xc1];
    assert!(rmp_serde::from_slice::<api::behavior::Event>(&corrupt).is_err());
    let wrong = rmp_serde::to_vec_named(&api::motion::ManualCommand {
        linear_x_mps: 0.1,
        angular_z_radps: 0.2,
    })
    .unwrap();
    assert!(rmp_serde::from_slice::<api::behavior::Snapshot>(&wrong).is_err());
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
            stamp: Some(::phoxal_bus::RobotInstant::new(
                ::phoxal_bus::TimelineId::from_raw(1).unwrap(),
                10,
            )),
        }],
    });
    round_trip(&api::power::State {
        status: api::power::Status::Idle,
        detail: None,
    });
    round_trip(&api::motion::State {
        manual_observed_age_ns: Some(10),
        autonomous_candidate_age_ns: None,
        safety_constraints_age_ns: Some(5),
        selected_source: Some(api::motion::Source::Manual),
        final_target: api::motion::Target {
            linear_x_mps: 0.1,
            angular_z_radps: 0.2,
            curvature_limit_radpm: None,
        },
        zero_reason: None,
        safety_runtime: api::motion::SafetyRuntime::Present,
        component_estop_blocked: false,
        active_safety_constraints: Vec::new(),
    });
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
    round_trip(&api::bus::uplink::State {
        phase: api::bus::uplink::UplinkPhase::Retrying,
        connect: Some("tls/root.example.io:7447".to_string()),
        retry_attempt: 3,
        detail: Some("connect failed".to_string()),
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
    round_trip(&api::perception::Detections {
        detections: vec![api::perception::Detection {
            class_id: "crate".to_string(),
            confidence: 0.8,
            position_m: [1.0, 2.0, 3.0],
            frame_id: "camera_link".to_string(),
            track_id: Some(6),
        }],
        stamp: Some(::phoxal_bus::RobotInstant::new(
            ::phoxal_bus::TimelineId::from_raw(1).unwrap(),
            7,
        )),
    });
    round_trip(&api::video::stream::StreamState {
        phase: api::video::stream::StreamPhase::Active,
        frames_seen: 12,
    });
}

#[test]
fn logs_event_defaults_truncation_for_pre_field_publishers() {
    #[derive(serde::Serialize)]
    struct LegacyLogEvent {
        seq: u64,
        time: api::logs::Timestamp,
        level: api::logs::Level,
        target: String,
        message: String,
        fields: std::collections::BTreeMap<String, api::logs::LogValue>,
        dropped: u32,
    }

    let bytes = rmp_serde::to_vec_named(&LegacyLogEvent {
        seq: 1,
        time: api::logs::Timestamp {
            unix_seconds: 2,
            nanos: 3,
        },
        level: api::logs::Level::Info,
        target: "legacy".to_string(),
        message: "old publisher".to_string(),
        fields: std::collections::BTreeMap::new(),
        dropped: 4,
    })
    .expect("encode legacy event");
    let decoded: api::logs::Event =
        rmp_serde::from_slice(&bytes).expect("decode additive event field");
    assert_eq!(decoded.dropped, 4);
    assert_eq!(decoded.truncated, 0);
}

#[test]
fn retained_tool_contracts_round_trip_through_messagepack() {
    round_trip(&api::tool::log::SnapshotRequest {});
    let cursor = api::tool::Cursor {
        generation: "opaque-generation".to_string(),
        sequence: 9,
    };
    let record = api::tool::log::Record {
        sequence: 9,
        participant_id: "drive".to_string(),
        source_sequence: 41,
        time: api::tool::log::Timestamp {
            unix_seconds: 1_800_000_000,
            nanos: 123,
        },
        level: api::tool::log::Level::Info,
        target: "drive".to_string(),
        message: "target accepted".to_string(),
        fields: [("speed".to_string(), api::tool::log::LogValue::F64(0.4))]
            .into_iter()
            .collect(),
        dropped: 0,
        truncated: 0,
    };
    round_trip(&api::tool::log::Snapshot {
        cursor: cursor.clone(),
        ingest_dropped: 2,
        records: vec![record.clone()],
    });
    round_trip(&api::tool::log::Follow {
        cursor: cursor.clone(),
        ingest_dropped: 2,
        record,
    });

    round_trip(&api::tool::bus::SnapshotRequest {});
    let window = api::tool::bus::Window {
        sequence: 9,
        topics: vec![api::tool::bus::TopicMetric {
            topic: "v0.1/drive/state".to_string(),
            from_participant: "drive".to_string(),
            ingress_rate_hz: 10.0,
            count: 42,
        }],
        throughput_msg_s: 12.5,
        window_ns: 1_000_000_000,
    };
    round_trip(&api::tool::bus::Snapshot {
        cursor: cursor.clone(),
        current: Some(window.clone()),
        windows: vec![window.clone()],
    });
    round_trip(&api::tool::bus::Follow {
        cursor: cursor.clone(),
        window,
    });

    let topic = api::tool::RuntimeTopic {
        topic: "v0.1/drive/state".to_string(),
        direction: api::tool::RuntimeDirection::Subscribe,
        buffer_kind: api::tool::RuntimeBufferKind::Latest,
        count: 42,
        rate_hz: 41.5,
        drops: 0,
        latest_overwrites: 41,
        bounded_evictions: 0,
        capacity: 1,
        current_depth: 1,
        high_water_depth: 1,
        decode_errors: 0,
        epoch_filtered: 0,
        overflowed_rows: 0,
    };
    let step = api::tool::RuntimeStep {
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
    round_trip(&api::tool::runtime::Rollup {
        window_ns: 1_000_000_000,
        step: Some(step.clone()),
        topics: vec![topic.clone()],
        overflow: None,
    });
    round_trip(&api::tool::runtime::SnapshotRequest {
        participant_id: Some("drive".to_string()),
        limit: 64,
        before_sequence: None,
    });
    let runtime_record = api::tool::runtime::Record {
        sequence: 10,
        participant_id: "drive".to_string(),
        truncated: 0,
        window_ns: 1_000_000_000,
        step: Some(step),
        topics: vec![topic],
        overflow: None,
    };
    round_trip(&api::tool::runtime::Snapshot {
        cursor: cursor.clone(),
        records: vec![runtime_record.clone()],
        capacity_evictions: 0,
        next_before_sequence: None,
    });
    round_trip(&api::tool::runtime::Follow {
        cursor,
        record: runtime_record,
    });

    let cursor = api::tool::Cursor {
        generation: "device-generation".to_string(),
        sequence: 9,
    };
    let sample = api::tool::device::Sample {
        cpu_pct: Some(12.5),
        ram_used_bytes: Some(1_073_741_824),
        ram_total_bytes: Some(17_179_869_184),
        swap_used_bytes: None,
        swap_total_bytes: None,
        load_1m: Some(0.75),
        load_5m: Some(0.5),
        load_15m: Some(0.25),
        uptime_s: Some(42),
        disks: Some(vec![api::tool::device::Disk {
            mount_point: "/".to_string(),
            file_system: "apfs".to_string(),
            used_bytes: 10,
            total_bytes: 100,
        }]),
        window_ns: 1_000_000_000,
    };
    let record = api::tool::device::Record {
        sequence: 9,
        sample: sample.clone(),
        truncated: 0,
    };
    round_trip(&sample);
    round_trip(&api::tool::device::SnapshotRequest {
        limit: 1,
        before_sequence: None,
    });
    round_trip(&api::tool::device::Snapshot {
        cursor: cursor.clone(),
        records: vec![record.clone()],
        capacity_evictions: 0,
        next_before_sequence: None,
    });
    round_trip(&api::tool::device::Follow { cursor, record });
}

#[test]
fn runtime_rollup_rejects_malformed_payloads() {
    let corrupt = [0xc1u8, 0xc1, 0xc1];
    assert!(rmp_serde::from_slice::<api::tool::runtime::Rollup>(&corrupt).is_err());

    let wrong_shape = rmp_serde::to_vec_named(&api::tool::log::SnapshotRequest {}).unwrap();
    assert!(rmp_serde::from_slice::<api::tool::runtime::Rollup>(&wrong_shape).is_err());
}

#[test]
fn folded_bodies_round_trip_through_messagepack() {
    // The clock body carries only the step counter now: its timeline and
    // instant ride in the envelope, stamped by the world authority.
    let clock = v0_1::simulation::Clock { step: 100 };
    assert_eq!(
        serde_json::to_value(&clock).unwrap(),
        serde_json::json!({ "step": 100_u64 })
    );
    round_trip(&clock);
    round_trip(&v0_1::joypad::Device {
        id: "xbox-controller-0".to_string(),
        name: "Xbox Wireless Controller".to_string(),
        status: v0_1::joypad::DeviceStatus::Ready,
    });
    round_trip(&v0_1::joypad::Devices {
        available: vec![v0_1::joypad::Device {
            id: "xbox-controller-0".to_string(),
            name: "Xbox Wireless Controller".to_string(),
            status: v0_1::joypad::DeviceStatus::Ready,
        }],
        selected: Some("xbox-controller-0".to_string()),
        enabled: false,
        unavailable_reason: None,
        last_error: None,
    });
    round_trip(&v0_1::joypad::Select {
        id: "xbox-controller-0".to_string(),
    });
    round_trip(&v0_1::joypad::SetEnabled { enabled: true });
    round_trip(&v0_1::joypad::Rescan {});
}

#[test]
fn session_joypad_state_rejects_malformed_payloads() {
    let corrupt = [0xc1u8, 0xc1, 0xc1];
    assert!(
        rmp_serde::from_slice::<v0_1::joypad::Devices>(&corrupt).is_err(),
        "corrupt MessagePack must not decode as joypad::Devices"
    );

    let wrong_shape = rmp_serde::to_vec_named(&v0_1::joypad::Select {
        id: "not-session-state".to_string(),
    })
    .unwrap();
    assert!(
        rmp_serde::from_slice::<v0_1::joypad::Devices>(&wrong_shape).is_err(),
        "a command body must not decode as joypad::Devices"
    );
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

#[test]
fn topic_builder_keys_match_contract_topics() {
    assert_eq!(api::topic::new().drive().state().key(), "v0.1/drive/state");
    assert_eq!(
        api::topic::new().drive().target().key(),
        "v0.1/drive/target"
    );
    assert_eq!(
        api::topic::new().navigation().state().key(),
        "v0.1/navigation/state"
    );
    assert_eq!(
        api::topic::new().navigation().request().key(),
        "v0.1/navigation/request"
    );
    assert_eq!(
        api::topic::new().navigation().result().key(),
        "v0.1/navigation/result"
    );
    assert_eq!(
        api::topic::new().behavior().request().key(),
        "v0.1/behavior/request"
    );
    assert_eq!(
        api::topic::new().behavior().event().key(),
        "v0.1/behavior/event"
    );
    assert_eq!(
        api::topic::new().safety().constraints().key(),
        "v0.1/safety/constraints"
    );
    assert_eq!(
        api::topic::new().safety().state().key(),
        "v0.1/safety/state"
    );
    assert_eq!(api::topic::new().frame().tree().key(), "v0.1/frame/tree");
    assert_eq!(
        api::topic::new().frame().static_transforms().key(),
        "v0.1/frame/static_transforms"
    );
    assert_eq!(
        api::topic::new().power().command().key(),
        "v0.1/power/command"
    );
    assert_eq!(
        api::topic::new().motion().manual().key(),
        "v0.1/motion/manual"
    );
    assert_eq!(
        api::topic::new().logs("drive").topic().key(),
        "v0.1/logs/drive"
    );
    assert_eq!(
        api::topic::new().bus().uplink().state().key(),
        "v0.1/bus/uplink/state"
    );
    assert_eq!(
        api::topic::new().tool().log().snapshot().key(),
        "v0.1/tool/log/snapshot"
    );
    assert_eq!(
        api::topic::new().tool().log().follow().key(),
        "v0.1/tool/log/follow"
    );
    assert_eq!(
        api::topic::new().tool().bus().snapshot().key(),
        "v0.1/tool/bus/snapshot"
    );
    assert_eq!(
        api::topic::new().tool().bus().follow().key(),
        "v0.1/tool/bus/follow"
    );
    assert_eq!(
        api::topic::new().tool().runtime().rollup().key(),
        "v0.1/tool/runtime/rollup"
    );
    assert_eq!(
        api::topic::new().tool().runtime().snapshot().key(),
        "v0.1/tool/runtime/snapshot"
    );
    assert_eq!(
        api::topic::new().tool().runtime().follow().key(),
        "v0.1/tool/runtime/follow"
    );
    assert_eq!(
        api::topic::new().perception().detections().key(),
        "v0.1/perception/detections"
    );
    assert_eq!(api::topic::new().video().open().key(), "v0.1/video/open");
    assert_eq!(
        api::topic::new().map().revision().key(),
        "v0.1/map/revision"
    );
    assert_eq!(api::topic::new().map().submap().key(), "v0.1/map/submap");
    assert_eq!(api::topic::new().asset().get().key(), "v0.1/asset/get");
    assert_eq!(
        api::topic::new().odometry().state().key(),
        "v0.1/odometry/state"
    );
    assert_eq!(
        api::topic::new().localize().state().key(),
        "v0.1/localize/state"
    );
}

#[test]
fn folded_topic_builder_keys_match_contract_topics() {
    assert_eq!(
        v0_1::topic::new().simulation().clock().key(),
        "v0.1/simulation/clock"
    );
    assert_eq!(
        v0_1::topic::new().joypad().devices().key(),
        "v0.1/joypad/devices"
    );
    assert_eq!(
        v0_1::topic::new().joypad().select().key(),
        "v0.1/joypad/select"
    );
    assert_eq!(
        v0_1::topic::new().joypad().set_enabled().key(),
        "v0.1/joypad/set_enabled"
    );
    assert_eq!(
        v0_1::topic::new().joypad().rescan().key(),
        "v0.1/joypad/rescan"
    );
}

#[test]
fn internal_owner_builder_produces_identical_keys() {
    // The OWNER (`internal`) builder mirrors the PUBLIC client builder's structure
    // and keys exactly (L1, plan #00) - only the leaf brand differs (verified by
    // the trybuild compile-fail/pass fixtures, not at runtime). The keys must match
    // the client side and the canonical contract topics.
    //
    // The owner entry requires the runner-minted `OwnerCap` (Layer 2); this test
    // mints one directly via the doc-hidden `__mint`, standing in for the runner.
    let cap = ::phoxal_bus::OwnerCap::__mint();
    assert_eq!(
        api::topic::internal::new(cap).drive().state().key(),
        "v0.1/drive/state"
    );
    assert_eq!(
        api::topic::internal::new(cap).drive().target().key(),
        "v0.1/drive/target"
    );
    assert_eq!(
        api::topic::internal::new(cap).tool().log().snapshot().key(),
        "v0.1/tool/log/snapshot"
    );
    assert_eq!(
        api::topic::internal::new(cap).tool().bus().follow().key(),
        "v0.1/tool/bus/follow"
    );
    assert_eq!(
        api::topic::internal::new(cap)
            .tool()
            .runtime()
            .rollup()
            .key(),
        "v0.1/tool/runtime/rollup"
    );
    assert_eq!(
        api::topic::internal::new(cap).map().submap().key(),
        "v0.1/map/submap"
    );
    assert_eq!(
        api::topic::internal::new(cap).video().open().key(),
        "v0.1/video/open"
    );
    assert_eq!(
        api::topic::internal::new(cap).logs("drive").topic().key(),
        "v0.1/logs/drive"
    );
    // Dynamic node path: the owner builder fills carried vars the same way.
    assert_eq!(
        api::topic::internal::new(cap).joint("elbow").state().key(),
        "v0.1/joint/elbow/state"
    );
    assert_eq!(
        api::topic::internal::new(cap)
            .video()
            .stream("front")
            .state()
            .key(),
        "v0.1/video/stream/front/state"
    );
}

// ---- dynamic / parameterized topics ----------------------------------------

#[test]
fn dynamic_topic_builder_fills_the_key_from_node_vars() {
    assert_eq!(
        api::topic::new().joint("elbow").state().key(),
        "v0.1/joint/elbow/state"
    );
    assert_eq!(
        api::topic::new().video().stream("front").state().key(),
        "v0.1/video/stream/front/state"
    );

    let topic = api::topic::new()
        .component("front_left_drive")
        .motor("motor")
        .command();
    assert_eq!(
        topic.key(),
        "v0.1/component/front_left_drive/motor/motor/command"
    );
    let enc = api::topic::new()
        .component("front_left_drive")
        .encoder("encoder")
        .sample();
    assert_eq!(
        enc.key(),
        "v0.1/component/front_left_drive/encoder/encoder/sample"
    );
}

#[test]
fn component_capability_topic_builders_fill_keys() {
    assert_eq!(
        api::topic::new()
            .component("imu0")
            .accelerometer("accel")
            .sample()
            .key(),
        "v0.1/component/imu0/accelerometer/accel/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("imu0")
            .gyroscope("gyro")
            .sample()
            .key(),
        "v0.1/component/imu0/gyroscope/gyro/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("imu0")
            .magnetometer("mag")
            .sample()
            .key(),
        "v0.1/component/imu0/magnetometer/mag/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("imu0")
            .imu("imu")
            .sample()
            .key(),
        "v0.1/component/imu0/imu/imu/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("base")
            .range("front_tof")
            .sample()
            .key(),
        "v0.1/component/base/range/front_tof/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("gps")
            .gnss("gnss")
            .sample()
            .key(),
        "v0.1/component/gps/gnss/gnss/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("head")
            .camera("front")
            .frame()
            .key(),
        "v0.1/component/head/camera/front/frame"
    );
    assert_eq!(
        api::topic::new()
            .component("head")
            .depth("front_depth")
            .frame()
            .key(),
        "v0.1/component/head/depth/front_depth/frame"
    );
    assert_eq!(
        api::topic::new()
            .component("front_lidar")
            .lidar("scan")
            .scan()
            .key(),
        "v0.1/component/front_lidar/lidar/scan/scan"
    );
    assert_eq!(
        api::topic::new()
            .component("radar")
            .mmwave("mmwave")
            .scan()
            .key(),
        "v0.1/component/radar/mmwave/mmwave/scan"
    );
    assert_eq!(
        api::topic::new()
            .component("head")
            .microphone("mic")
            .frame()
            .key(),
        "v0.1/component/head/microphone/mic/frame"
    );
    assert_eq!(
        api::topic::new()
            .component("status_panel")
            .led("status")
            .command()
            .key(),
        "v0.1/component/status_panel/led/status/command"
    );
    assert_eq!(
        api::topic::new()
            .component("safety_panel")
            .emergency_stop("estop")
            .state()
            .key(),
        "v0.1/component/safety_panel/emergency_stop/estop/state"
    );
}

#[test]
fn dynamic_topic_contract_body_topic_is_derived_from_node_path() {
    assert_eq!(
        <api::component::motor::Command as ContractBody>::TOPIC,
        "v0.1/component/{instance}/motor/{capability}/command"
    );
    assert_eq!(
        <api::component::imu::Sample as ContractBody>::TOPIC,
        "v0.1/component/{instance}/imu/{capability}/sample"
    );
    assert_eq!(
        <api::component::camera::Frame as ContractBody>::TOPIC,
        "v0.1/component/{instance}/camera/{capability}/frame"
    );
    assert_eq!(
        <api::component::encoder::Sample as ContractBody>::TOPIC,
        "v0.1/component/{instance}/encoder/{capability}/sample"
    );
}

#[test]
fn dynamic_topic_wildcard_is_subscribe_only() {
    let concrete = api::topic::new().component("base").motor("motor").command();
    assert!(concrete.publish_key().is_ok());

    let wildcard = api::topic::new().component("*").motor("motor").command();
    assert_eq!(wildcard.key(), "v0.1/component/*/motor/motor/command");
    assert!(wildcard.publish_key().is_err());
}

fn round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = rmp_serde::to_vec_named(value).unwrap();
    let decoded: T = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(value, &decoded);
}

// A nested dynamic tree that reuses the same var name across levels. This module
// must *compile*: it proves the builder's positional field storage (`__seg0`,
// `__seg1`) does not collide into duplicate struct fields, and that each level's
// value is carried independently (a regression would fail to build or collapse the
// key).
mod reused_var_name {
    crate::phoxal_api_tree! {
        version v0_1 {
            outer(id) {
                inner(id) {
                    struct Body { x: u8 }
                    topic event: state Body;
                }
            }
        }
        latest v0_1;
    }

    #[test]
    fn nested_reused_var_carries_each_level_independently() {
        let topic = v0_1::topic::new().outer("a").inner("b").event();
        assert_eq!(topic.key(), "v0.1/outer/a/inner/b/event");
    }
}

// A standalone API revision used to exercise the macro independently of the
// production tree and prove nested invocations remain self-contained.
mod standalone_version {
    use crate::{ApiVersion, ContractBody};

    crate::phoxal_api_tree! {
        version v0_1 {
            sample {
                struct Body { value: u8, note: Option<String> }
                topic body: state Body;
            }
        }
        latest v0_1;
    }

    #[test]
    fn standalone_version_expands_and_round_trips() {
        assert_eq!(<v0_1::Api as ApiVersion>::ID, "v0.1");
        assert_eq!(
            <v0_1::sample::Body as ContractBody>::TOPIC,
            "v0.1/sample/body"
        );
        assert_eq!(v0_1::topic::new().sample().body().key(), "v0.1/sample/body");

        let body = v0_1::sample::Body {
            value: 7,
            note: Some("standalone".to_string()),
        };
        let bytes = rmp_serde::to_vec_named(&body).unwrap();
        let decoded: v0_1::sample::Body = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(body, decoded);
    }
}
