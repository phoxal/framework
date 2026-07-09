//! Golden tests for the generated API layer: the version-local body shape (plain
//! payload, no `{"v":…}` wrapper — D62), the `ContractBody` consts, the
//! `ApiVersion` id, and the topic keys produced by the api-local builders.

use crate::y2026_1 as api;
use crate::y2026_7;
use crate::{ApiVersion, ContractBody};
use phoxal_bus::TopicRole;

#[test]
fn api_version_id_is_the_dated_module_name() {
    assert_eq!(<api::Api as ApiVersion>::ID, "y2026_1");
    const { assert!(!<api::Api as ApiVersion>::IS_PREVIEW) };
}

#[test]
fn contract_body_topic_is_generation_qualified() {
    // D1: the generation is folded into the wire key, so two participants using
    // the same version-qualified contract share a key that cannot collide with
    // any other generation's contract of the same leaf name.
    assert_eq!(
        <api::drive::State as ContractBody>::TOPIC,
        "y2026_1/drive/state"
    );
    assert_eq!(
        <api::drive::Target as ContractBody>::TOPIC,
        "y2026_1/drive/target"
    );
    assert_eq!(
        <api::safety::Status as ContractBody>::TOPIC,
        "y2026_1/safety/state"
    );
    assert_eq!(
        <api::safety::SafetyAuthorization as ContractBody>::TOPIC,
        "y2026_1/safety/authorization"
    );
    assert_eq!(
        <api::mission::State as ContractBody>::TOPIC,
        "y2026_1/mission/state"
    );
    assert_eq!(
        <api::joint::JointState as ContractBody>::TOPIC,
        "y2026_1/joint/{joint}/state"
    );
    assert_eq!(
        <api::video::stream::StreamState as ContractBody>::TOPIC,
        "y2026_1/video/stream/{stream}/state"
    );
    assert_eq!(
        <api::localize::LocalizationState as ContractBody>::TOPIC,
        "y2026_1/localize/state"
    );
    assert_eq!(
        <api::logs::Event as ContractBody>::TOPIC,
        "y2026_1/logs/{participant_id}"
    );
    assert_eq!(
        <api::bus::uplink::State as ContractBody>::TOPIC,
        "y2026_1/bus/uplink/state"
    );
}

#[test]
fn y2026_7_is_a_standalone_second_generation_carrying_only_the_moved_battery_contract() {
    // The ground-breaker: `battery::State` moved OUT of y2026_1 and into its
    // own, sparse y2026_7 generation (D1 - no `extends`, no copy of y2026_1).
    const { assert!(!<y2026_7::Api as ApiVersion>::IS_PREVIEW) };
    assert_eq!(<y2026_7::Api as ApiVersion>::ID, "y2026_7");
    assert_eq!(
        <y2026_7::battery::State as ContractBody>::TOPIC,
        "y2026_7/battery/state"
    );
    assert_eq!(
        y2026_7::topic::new().battery().state().key(),
        "y2026_7/battery/state"
    );
}

#[test]
fn generated_contract_manifest_lists_contract_shapes() {
    let generation = crate::API_CONTRACT_MANIFEST
        .iter()
        .find(|generation| generation.name == "y2026_1")
        .expect("y2026_1 should be in the generated manifest");
    assert!(!generation.is_preview);

    let drive_state = generation
        .contracts
        .iter()
        .find(|contract| contract.family == "y2026_1::drive::State")
        .expect("drive::State should be in the generated manifest");
    assert_eq!(drive_state.topic, "y2026_1/drive/state");

    // The manifest also carries the second, standalone generation, mixed-in
    // alongside y2026_1 - the multi-generation catalog proof (task step 5).
    let y2026_7_generation = crate::API_CONTRACT_MANIFEST
        .iter()
        .find(|generation| generation.name == "y2026_7")
        .expect("y2026_7 should be in the generated manifest");
    assert!(!y2026_7_generation.is_preview);
    let battery_state = y2026_7_generation
        .contracts
        .iter()
        .find(|contract| contract.family == "y2026_7::battery::State")
        .expect("battery::State should be in the y2026_7 manifest entry");
    assert_eq!(battery_state.topic, "y2026_7/battery/state");
}

#[test]
fn generated_role_const_matches_each_topic_role() {
    // The `ROLE` const is an inherent const generated per body by
    // `phoxal_api_tree!`. Assert representative topics across all three roles so a
    // generator bug that emitted an all-`State` (or all-`Command`) tree is caught
    // by `cargo test`, without relying on any external script.

    // Command: a control input the owning service subscribes.
    assert_eq!(api::drive::Target::ROLE, TopicRole::Command);
    assert_eq!(api::power::Command::ROLE, TopicRole::Command);

    // State: telemetry the owning service publishes.
    assert_eq!(api::drive::State::ROLE, TopicRole::State);
    assert_eq!(y2026_7::battery::State::ROLE, TopicRole::State);

    // Query: both the request and the response body of a request/response topic
    // carry the `Query` role.
    assert_eq!(api::frame::LookupRequest::ROLE, TopicRole::Query);
    assert_eq!(api::frame::LookupResponse::ROLE, TopicRole::Query);
    assert_eq!(api::map::SubmapRequest::ROLE, TopicRole::Query);
    assert_eq!(api::map::SubmapResponse::ROLE, TopicRole::Query);
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
fn new_y2026_1_family_bodies_round_trip_through_messagepack() {
    let authorization = api::safety::SafetyAuthorization {
        decision: api::safety::SafetyDecision::Slow,
        approved_motion: api::safety::MotionConstraint {
            linear_x_mps: api::safety::Constraint {
                min: -0.1,
                max: 0.1,
            },
            angular_z_radps: api::safety::Constraint {
                min: -0.5,
                max: 0.5,
            },
        },
        reasons: vec![api::safety::SafetyReason {
            code: api::safety::SafetyReasonCode::BatteryLow,
            detail: Some("pack below low threshold".to_string()),
        }],
        source_revision: api::safety::SafetySourceRevision {
            localization: Some(7),
            map: Some(9),
        },
        expires_at_ns: Some(42),
    };
    round_trip(&authorization);

    round_trip(&api::mission::State {
        phase: api::mission::Phase::Active,
        goal: Some(api::mission::Goal {
            x_m: 1.0,
            y_m: 2.0,
            yaw_rad: Some(0.25),
        }),
        detail: None,
    });
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
            stamp_ns: Some(10),
        }],
    });
    round_trip(&api::power::State {
        status: api::power::Status::Idle,
        detail: None,
    });
    round_trip(&api::motion::State {
        active_source: Some(api::motion::MotionSource::Manual),
        selected: Some(api::motion::Target {
            linear_x_mps: 0.1,
            angular_z_radps: 0.2,
            curvature_limit_radpm: None,
        }),
        reason: None,
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
    });
    round_trip(&api::bus::uplink::State {
        phase: api::bus::uplink::UplinkPhase::Retrying,
        connect: Some("tls/root.example.io:7447".to_string()),
        retry_attempt: 3,
        detail: Some("connect failed".to_string()),
    });
    round_trip(&api::plan::Path {
        poses: vec![api::plan::PathPose {
            x_m: 1.0,
            y_m: 2.0,
            yaw_rad: None,
        }],
        map_revision: Some(3),
    });
    round_trip(&api::follow::State {
        active: true,
        target_index: Some(4),
        finished: false,
    });
    round_trip(&api::explore::Frontiers {
        frontiers: vec![api::explore::Frontier {
            x_m: 1.0,
            y_m: 2.0,
            size: 12,
            score: 0.75,
        }],
        map_revision: Some(5),
    });
    round_trip(&api::perception::Detections {
        detections: vec![api::perception::Detection {
            class_id: "crate".to_string(),
            confidence: 0.8,
            position_m: [1.0, 2.0, 3.0],
            frame_id: "camera_link".to_string(),
            track_id: Some(6),
        }],
        stamp_ns: Some(7),
    });
    round_trip(&api::video::stream::StreamState {
        phase: api::video::stream::StreamPhase::Active,
        frames_seen: 12,
    });
    round_trip(&api::simulation::RobotPose {
        x_m: 1.0,
        y_m: 2.0,
        yaw_rad: 0.3,
    });
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
        measured_at_ns: Some(42),
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
        measured_at_ns: Some(300),
        calibration: Some(api::component::camera::CalibrationIdentity {
            id: "front".to_string(),
            version: "v1".to_string(),
        }),
        data: vec![1, 2, 3, 4],
    };
    let bytes = rmp_serde::to_vec_named(&frame).unwrap();
    let decoded: api::component::camera::Frame = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn topic_builder_keys_match_contract_topics() {
    assert_eq!(
        api::topic::new().drive().state().key(),
        "y2026_1/drive/state"
    );
    assert_eq!(
        api::topic::new().drive().target().key(),
        "y2026_1/drive/target"
    );
    assert_eq!(
        api::topic::new().safety().state().key(),
        "y2026_1/safety/state"
    );
    assert_eq!(
        api::topic::new().safety().authorization().key(),
        "y2026_1/safety/authorization"
    );
    assert_eq!(
        api::topic::new().mission().state().key(),
        "y2026_1/mission/state"
    );
    assert_eq!(api::topic::new().frame().tree().key(), "y2026_1/frame/tree");
    assert_eq!(
        api::topic::new().frame().static_transforms().key(),
        "y2026_1/frame/static_transforms"
    );
    assert_eq!(
        api::topic::new().power().command().key(),
        "y2026_1/power/command"
    );
    assert_eq!(
        api::topic::new().motion().manual().key(),
        "y2026_1/motion/manual"
    );
    assert_eq!(
        api::topic::new().logs("drive").topic().key(),
        "y2026_1/logs/drive"
    );
    assert_eq!(
        api::topic::new().bus().uplink().state().key(),
        "y2026_1/bus/uplink/state"
    );
    assert_eq!(api::topic::new().plan().path().key(), "y2026_1/plan/path");
    assert_eq!(
        api::topic::new().follow().state().key(),
        "y2026_1/follow/state"
    );
    assert_eq!(
        api::topic::new().explore().frontiers().key(),
        "y2026_1/explore/frontiers"
    );
    assert_eq!(
        api::topic::new().perception().detections().key(),
        "y2026_1/perception/detections"
    );
    assert_eq!(
        api::topic::new().simulation().robot_pose().key(),
        "y2026_1/simulation/robot_pose"
    );
    assert_eq!(api::topic::new().video().open().key(), "y2026_1/video/open");
    assert_eq!(
        api::topic::new().presence().heartbeat().key(),
        "y2026_1/presence/heartbeat"
    );
    assert_eq!(
        api::topic::new().map().revision().key(),
        "y2026_1/map/revision"
    );
    assert_eq!(api::topic::new().map().submap().key(), "y2026_1/map/submap");
    assert_eq!(api::topic::new().asset().get().key(), "y2026_1/asset/get");
    assert_eq!(
        api::topic::new().odometry().state().key(),
        "y2026_1/odometry/state"
    );
    assert_eq!(
        api::topic::new().localize().state().key(),
        "y2026_1/localize/state"
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
        "y2026_1/drive/state"
    );
    assert_eq!(
        api::topic::internal::new(cap).drive().target().key(),
        "y2026_1/drive/target"
    );
    assert_eq!(
        api::topic::internal::new(cap).map().submap().key(),
        "y2026_1/map/submap"
    );
    assert_eq!(
        api::topic::internal::new(cap).video().open().key(),
        "y2026_1/video/open"
    );
    assert_eq!(
        api::topic::internal::new(cap).logs("drive").topic().key(),
        "y2026_1/logs/drive"
    );
    // Dynamic node path: the owner builder fills carried vars the same way.
    assert_eq!(
        api::topic::internal::new(cap).joint("elbow").state().key(),
        "y2026_1/joint/elbow/state"
    );
    assert_eq!(
        api::topic::internal::new(cap)
            .video()
            .stream("front")
            .state()
            .key(),
        "y2026_1/video/stream/front/state"
    );
}

// ---- dynamic / parameterized topics ----------------------------------------

#[test]
fn dynamic_topic_builder_fills_the_key_from_node_vars() {
    assert_eq!(
        api::topic::new().joint("elbow").state().key(),
        "y2026_1/joint/elbow/state"
    );
    assert_eq!(
        api::topic::new().video().stream("front").state().key(),
        "y2026_1/video/stream/front/state"
    );

    let topic = api::topic::new()
        .component("front_left_drive")
        .motor("motor")
        .command();
    assert_eq!(
        topic.key(),
        "y2026_1/component/front_left_drive/motor/motor/command"
    );
    let enc = api::topic::new()
        .component("front_left_drive")
        .encoder("encoder")
        .sample();
    assert_eq!(
        enc.key(),
        "y2026_1/component/front_left_drive/encoder/encoder/sample"
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
        "y2026_1/component/imu0/accelerometer/accel/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("imu0")
            .gyroscope("gyro")
            .sample()
            .key(),
        "y2026_1/component/imu0/gyroscope/gyro/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("imu0")
            .magnetometer("mag")
            .sample()
            .key(),
        "y2026_1/component/imu0/magnetometer/mag/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("imu0")
            .imu("imu")
            .sample()
            .key(),
        "y2026_1/component/imu0/imu/imu/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("base")
            .range("front_tof")
            .sample()
            .key(),
        "y2026_1/component/base/range/front_tof/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("gps")
            .gnss("gnss")
            .sample()
            .key(),
        "y2026_1/component/gps/gnss/gnss/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("head")
            .camera("front")
            .frame()
            .key(),
        "y2026_1/component/head/camera/front/frame"
    );
    assert_eq!(
        api::topic::new()
            .component("head")
            .depth("front_depth")
            .frame()
            .key(),
        "y2026_1/component/head/depth/front_depth/frame"
    );
    assert_eq!(
        api::topic::new()
            .component("front_lidar")
            .lidar("scan")
            .scan()
            .key(),
        "y2026_1/component/front_lidar/lidar/scan/scan"
    );
    assert_eq!(
        api::topic::new()
            .component("radar")
            .mmwave("mmwave")
            .scan()
            .key(),
        "y2026_1/component/radar/mmwave/mmwave/scan"
    );
    assert_eq!(
        api::topic::new()
            .component("head")
            .microphone("mic")
            .frame()
            .key(),
        "y2026_1/component/head/microphone/mic/frame"
    );
    assert_eq!(
        api::topic::new()
            .component("status_panel")
            .led("status")
            .command()
            .key(),
        "y2026_1/component/status_panel/led/status/command"
    );
    assert_eq!(
        api::topic::new()
            .component("safety_panel")
            .emergency_stop("estop")
            .state()
            .key(),
        "y2026_1/component/safety_panel/emergency_stop/estop/state"
    );
}

#[test]
fn dynamic_topic_contract_body_topic_is_derived_from_node_path() {
    assert_eq!(
        <api::component::motor::Command as ContractBody>::TOPIC,
        "y2026_1/component/{instance}/motor/{capability}/command"
    );
    assert_eq!(
        <api::component::imu::Sample as ContractBody>::TOPIC,
        "y2026_1/component/{instance}/imu/{capability}/sample"
    );
    assert_eq!(
        <api::component::camera::Frame as ContractBody>::TOPIC,
        "y2026_1/component/{instance}/camera/{capability}/frame"
    );
    assert_eq!(
        <api::component::encoder::Sample as ContractBody>::TOPIC,
        "y2026_1/component/{instance}/encoder/{capability}/sample"
    );
}

#[test]
fn dynamic_topic_wildcard_is_subscribe_only() {
    let concrete = api::topic::new().component("base").motor("motor").command();
    assert!(concrete.publish_key().is_ok());

    let wildcard = api::topic::new().component("*").motor("motor").command();
    assert_eq!(wildcard.key(), "y2026_1/component/*/motor/motor/command");
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
        version rv {
            outer(id) {
                inner(id) {
                    struct Body { x: u8 }
                    topic event: state Body;
                }
            }
        }
    }

    #[test]
    fn nested_reused_var_carries_each_level_independently() {
        let topic = rv::topic::new().outer("a").inner("b").event();
        assert_eq!(topic.key(), "rv/outer/a/inner/b/event");
    }
}

// A standalone second generation with no parent (there is no `extends`, D1):
// a sparse batch that mints its own contract, fully independent of y2026_1.
//
// This does NOT use `preview` here: a `preview` version is feature-gated
// (`preview-y2026_N`), and that feature must be registered in this crate's
// `@generated` `[features]` block in `Cargo.toml` (written by
// `xtask api sync-features`, out of scope for this slice) before a preview
// module actually compiles in. The `preview` marker itself - standalone, with
// no `extends` - is exercised at the token level in
// `phoxal-macros/src/api_tree.rs`'s
// `standalone_preview_version_emits_final_path_feature_gate_and_lifecycle_const`
// test, which does not need the feature wired.
mod standalone_second_generation {
    use crate::{ApiVersion, ContractBody};

    crate::phoxal_api_tree! {
        version y2026_2 {
            sample {
                struct Body { value: u8, note: Option<String> }
                topic body: state Body;
            }
        }
    }

    #[test]
    fn standalone_generation_expands_and_round_trips() {
        const { assert!(!<y2026_2::Api as ApiVersion>::IS_PREVIEW) };
        assert_eq!(<y2026_2::Api as ApiVersion>::ID, "y2026_2");
        assert_eq!(
            <y2026_2::sample::Body as ContractBody>::TOPIC,
            "y2026_2/sample/body"
        );
        assert_eq!(
            y2026_2::topic::new().sample().body().key(),
            "y2026_2/sample/body"
        );

        let body = y2026_2::sample::Body {
            value: 7,
            note: Some("standalone".to_string()),
        };
        let bytes = rmp_serde::to_vec_named(&body).unwrap();
        let decoded: y2026_2::sample::Body = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(body, decoded);
    }
}
