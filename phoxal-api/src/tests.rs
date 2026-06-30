//! Golden tests for the generated API layer: the version-local body shape (plain
//! payload, no `{"v":…}` wrapper — D62), the `ContractBody` consts, the
//! `ApiVersion` id, and the topic keys produced by the api-local builders.

use crate::y2026_1 as api;
use crate::{ApiVersion, ContractBody};

#[test]
fn api_version_id_is_the_dated_module_name() {
    assert_eq!(<api::Api as ApiVersion>::ID, "y2026_1");
}

#[test]
fn contract_body_consts_are_family_and_topic() {
    assert_eq!(<api::drive::State as ContractBody>::FAMILY, "drive::State");
    assert_eq!(<api::drive::State as ContractBody>::TOPIC, "drive/state");
    assert_eq!(
        <api::drive::Target as ContractBody>::FAMILY,
        "drive::Target"
    );
    assert_eq!(<api::drive::Target as ContractBody>::TOPIC, "drive/target");
    assert_eq!(
        <api::battery::State as ContractBody>::TOPIC,
        "battery/state"
    );
    assert_eq!(<api::safety::Status as ContractBody>::TOPIC, "safety/state");
    assert_eq!(
        <api::safety::SafetyAuthorization as ContractBody>::TOPIC,
        "safety/authorization"
    );
    assert_eq!(
        <api::mission::State as ContractBody>::TOPIC,
        "mission/state"
    );
    assert_eq!(
        <api::joint::JointState as ContractBody>::TOPIC,
        "joint/{joint}/state"
    );
    assert_eq!(
        <api::joint::JointState as ContractBody>::FAMILY,
        "joint::JointState"
    );
    assert_eq!(
        <api::video::stream::StreamEvent as ContractBody>::TOPIC,
        "video/stream/{stream}/event"
    );
    assert_eq!(
        <api::video::stream::StreamEvent as ContractBody>::FAMILY,
        "video::stream::StreamEvent"
    );
    assert_eq!(
        <api::localize::LocalizationState as ContractBody>::TOPIC,
        "localize/state"
    );
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
    round_trip(&api::video::stream::StreamEvent::KeyFrame);
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
    assert_eq!(api::topic::new().drive().state().key(), "drive/state");
    assert_eq!(api::topic::new().drive().target().key(), "drive/target");
    assert_eq!(api::topic::new().battery().state().key(), "battery/state");
    assert_eq!(api::topic::new().safety().state().key(), "safety/state");
    assert_eq!(
        api::topic::new().safety().authorization().key(),
        "safety/authorization"
    );
    assert_eq!(api::topic::new().mission().state().key(), "mission/state");
    assert_eq!(api::topic::new().frame().tree().key(), "frame/tree");
    assert_eq!(
        api::topic::new().frame().static_transforms().key(),
        "frame/static_transforms"
    );
    assert_eq!(api::topic::new().power().command().key(), "power/command");
    assert_eq!(api::topic::new().motion().manual().key(), "motion/manual");
    assert_eq!(api::topic::new().plan().path().key(), "plan/path");
    assert_eq!(api::topic::new().follow().state().key(), "follow/state");
    assert_eq!(
        api::topic::new().explore().frontiers().key(),
        "explore/frontiers"
    );
    assert_eq!(
        api::topic::new().perception().detections().key(),
        "perception/detections"
    );
    assert_eq!(
        api::topic::new().simulation().robot_pose().key(),
        "simulation/robot_pose"
    );
    assert_eq!(api::topic::new().video().open().key(), "video/open");
    assert_eq!(
        api::topic::new().presence().heartbeat().key(),
        "presence/heartbeat"
    );
    assert_eq!(api::topic::new().map().revision().key(), "map/revision");
    assert_eq!(api::topic::new().map().submap().key(), "map/submap");
    assert_eq!(api::topic::new().asset().get().key(), "asset/get");
    assert_eq!(api::topic::new().odometry().state().key(), "odometry/state");
    assert_eq!(api::topic::new().localize().state().key(), "localize/state");
}

// ---- dynamic / parameterized topics ----------------------------------------

#[test]
fn dynamic_topic_builder_fills_the_key_from_node_vars() {
    assert_eq!(
        api::topic::new().joint("elbow").state().key(),
        "joint/elbow/state"
    );
    assert_eq!(
        api::topic::new().video().stream("front").event().key(),
        "video/stream/front/event"
    );

    let topic = api::topic::new()
        .component("front_left_drive")
        .motor("motor")
        .command();
    assert_eq!(
        topic.key(),
        "component/front_left_drive/motor/motor/command"
    );
    let enc = api::topic::new()
        .component("front_left_drive")
        .encoder("encoder")
        .sample();
    assert_eq!(
        enc.key(),
        "component/front_left_drive/encoder/encoder/sample"
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
        "component/imu0/accelerometer/accel/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("imu0")
            .gyroscope("gyro")
            .sample()
            .key(),
        "component/imu0/gyroscope/gyro/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("imu0")
            .magnetometer("mag")
            .sample()
            .key(),
        "component/imu0/magnetometer/mag/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("imu0")
            .imu("imu")
            .sample()
            .key(),
        "component/imu0/imu/imu/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("base")
            .range("front_tof")
            .sample()
            .key(),
        "component/base/range/front_tof/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("gps")
            .gnss("gnss")
            .sample()
            .key(),
        "component/gps/gnss/gnss/sample"
    );
    assert_eq!(
        api::topic::new()
            .component("head")
            .camera("front")
            .frame()
            .key(),
        "component/head/camera/front/frame"
    );
    assert_eq!(
        api::topic::new()
            .component("head")
            .depth("front_depth")
            .frame()
            .key(),
        "component/head/depth/front_depth/frame"
    );
    assert_eq!(
        api::topic::new()
            .component("front_lidar")
            .lidar("scan")
            .scan()
            .key(),
        "component/front_lidar/lidar/scan/scan"
    );
    assert_eq!(
        api::topic::new()
            .component("radar")
            .mmwave("mmwave")
            .scan()
            .key(),
        "component/radar/mmwave/mmwave/scan"
    );
    assert_eq!(
        api::topic::new()
            .component("head")
            .microphone("mic")
            .frame()
            .key(),
        "component/head/microphone/mic/frame"
    );
    assert_eq!(
        api::topic::new()
            .component("status_panel")
            .led("status")
            .command()
            .key(),
        "component/status_panel/led/status/command"
    );
    assert_eq!(
        api::topic::new()
            .component("safety_panel")
            .emergency_stop("estop")
            .state()
            .key(),
        "component/safety_panel/emergency_stop/estop/state"
    );
}

#[test]
fn dynamic_topic_contract_body_topic_is_derived_from_node_path() {
    assert_eq!(
        <api::component::motor::Command as ContractBody>::TOPIC,
        "component/{instance}/motor/{capability}/command"
    );
    assert_eq!(
        <api::component::motor::Command as ContractBody>::FAMILY,
        "component::motor::Command"
    );
    assert_eq!(
        <api::component::imu::Sample as ContractBody>::TOPIC,
        "component/{instance}/imu/{capability}/sample"
    );
    assert_eq!(
        <api::component::imu::Sample as ContractBody>::FAMILY,
        "component::imu::Sample"
    );
    assert_eq!(
        <api::component::camera::Frame as ContractBody>::TOPIC,
        "component/{instance}/camera/{capability}/frame"
    );
    assert_eq!(
        <api::component::camera::Frame as ContractBody>::FAMILY,
        "component::camera::Frame"
    );
    assert_eq!(
        <api::component::encoder::Sample as ContractBody>::TOPIC,
        "component/{instance}/encoder/{capability}/sample"
    );
    assert_eq!(
        <api::component::encoder::Sample as ContractBody>::FAMILY,
        "component::encoder::Sample"
    );
}

#[test]
fn dynamic_topic_wildcard_is_subscribe_only() {
    let concrete = api::topic::new().component("base").motor("motor").command();
    assert!(concrete.publish_key().is_ok());

    let wildcard = api::topic::new().component("*").motor("motor").command();
    assert_eq!(wildcard.key(), "component/*/motor/motor/command");
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

// A self-contained three-version tree exercising multi-level inheritance
// (`vc extends vb extends va`) without touching the production api tree.
mod multi_level {
    use crate::{ApiVersion, ContractBody};

    crate::phoxal_api_tree! {
        version va {
            sensor {
                struct Reading { value_a: f32 }
                topic reading: pubsub Reading;
            }
        }
        version vb extends va {
            beacon {
                struct Ping { seq: u64 }
                topic ping: pubsub Ping;
            }
        }
        version vc extends vb {
            sensor {
                struct Reading { value_a: f32, value_b: f32 }
                topic reading: pubsub Reading;
            }
        }
    }

    #[test]
    fn transitive_inheritance_carries_through_two_levels() {
        // `beacon` is declared in vb and inherited by vc (two levels deep).
        assert_eq!(<vc::beacon::Ping as ContractBody>::TOPIC, "beacon/ping");
        assert_eq!(<vc::beacon::Ping as ContractBody>::FAMILY, "beacon::Ping");
        assert_eq!(
            <<vc::beacon::Ping as ContractBody>::Api as ApiVersion>::ID,
            "vc"
        );

        // `sensor::Reading` is inherited unchanged into vb, then overridden in vc.
        assert_eq!(
            <vb::sensor::Reading as ContractBody>::TOPIC,
            "sensor/reading"
        );
        let _vc_reading = vc::sensor::Reading {
            value_a: 1.0,
            value_b: 2.0,
        };
        assert_eq!(vc::topic::new().beacon().ping().key(), "beacon/ping");
        assert_eq!(vc::topic::new().sensor().reading().key(), "sensor/reading");
    }
}

// A self-contained two-version tree exercising `extends` over a *nested,
// dynamic* node subtree: the child inherits the whole `component(instance)`
// subtree, overrides one leaf type, and appends a sibling node — every inherited
// type re-emits fresh under the child `Api` (D61).
mod extends_nested {
    use crate::{ApiVersion, ContractBody};

    crate::phoxal_api_tree! {
        version base {
            component(instance) {
                motor(capability) {
                    enum Command { Velocity(f32), Stop }
                    topic command: pubsub Command;
                }
                imu(capability) {
                    struct Sample { angular_velocity_radps: [f32; 3] }
                    topic sample: pubsub Sample;
                }
            }
        }
        version next extends base {
            // Override only the nested `motor::Command` body; everything else in the
            // `component` subtree (including `imu`) is inherited unchanged.
            component(instance) {
                motor(capability) {
                    enum Command { Velocity(f32), Torque(f32), Stop }
                    topic command: pubsub Command;
                }
            }
            // A brand-new top-level node added in the child version.
            battery {
                struct State { soc: f32 }
                topic state: pubsub State;
            }
        }
    }

    #[test]
    fn nested_dynamic_subtree_is_inherited_and_overlaid() {
        // The whole nested subtree is inherited: keys/families are byte-identical
        // across versions, only the `Api` marker differs.
        assert_eq!(
            <next::component::motor::Command as ContractBody>::TOPIC,
            "component/{instance}/motor/{capability}/command"
        );
        assert_eq!(
            <next::component::motor::Command as ContractBody>::FAMILY,
            "component::motor::Command"
        );
        assert_eq!(
            <next::component::imu::Sample as ContractBody>::TOPIC,
            "component/{instance}/imu/{capability}/sample"
        );
        assert_eq!(
            <next::component::imu::Sample as ContractBody>::FAMILY,
            "component::imu::Sample"
        );
        assert_eq!(
            <<next::component::imu::Sample as ContractBody>::Api as ApiVersion>::ID,
            "next"
        );

        // The override took: the child `motor::Command` carries the new `Torque`
        // variant that the base did not have.
        let _overridden = next::component::motor::Command::Torque(1.0);

        // The brand-new `battery` node added in the child is present.
        assert_eq!(
            <next::battery::State as ContractBody>::TOPIC,
            "battery/state"
        );

        // The builder chain reproduces the same nested keys under both versions.
        assert_eq!(
            base::topic::new()
                .component("front")
                .imu("imu0")
                .sample()
                .key(),
            "component/front/imu/imu0/sample"
        );
        assert_eq!(
            next::topic::new()
                .component("front")
                .motor("m0")
                .command()
                .key(),
            "component/front/motor/m0/command"
        );
        assert_eq!(next::topic::new().battery().state().key(), "battery/state");
    }
}

// A nested dynamic tree that REUSES the same var name across levels. This module
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
                    topic event: pubsub Body;
                }
            }
        }
    }

    #[test]
    fn nested_reused_var_carries_each_level_independently() {
        let topic = rv::topic::new().outer("a").inner("b").event();
        assert_eq!(topic.key(), "outer/a/inner/b/event");
    }
}
