//! Golden tests for the generated API layer: the version-local body shape (plain
//! payload, no `{"v":…}` wrapper — D62), the `ContractBody` consts, the
//! `ApiVersion` id, and the topic keys produced by the api-local builders.

use crate::api::y2026_1 as api;
use crate::api::{ApiVersion, ContractBody};

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
        <api::motor::Command as ContractBody>::TOPIC,
        "motor/command"
    );
    assert_eq!(
        <api::battery::State as ContractBody>::TOPIC,
        "battery/state"
    );
    assert_eq!(<api::safety::Status as ContractBody>::TOPIC, "safety/state");
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
fn recovered_component_capability_bodies_round_trip_through_messagepack() {
    let imu = api::component::ImuSample {
        orientation: Some([1.0, 0.0, 0.0, 0.0]),
        angular_velocity_radps: [0.1, 0.2, 0.3],
        linear_acceleration_mps2: [1.0, 2.0, 9.81],
        covariance: Some([0.0; 9]),
        noise_density: Some([0.01, 0.02, 0.03]),
        sensor_frame_id: Some("imu_link".to_string()),
        measured_at_ns: Some(42),
        health: api::component::SensorHealth::Degraded,
        bias: Some(api::component::Bias {
            angular_velocity_radps: [0.001, 0.002, 0.003],
            linear_acceleration_mps2: [0.01, 0.02, 0.03],
        }),
    };
    let bytes = rmp_serde::to_vec_named(&imu).unwrap();
    let decoded: api::component::ImuSample = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(imu, decoded);

    let frame = api::component::CameraFrame {
        width: 640,
        height: 480,
        encoding: api::component::CameraEncoding::Rgb8,
        intrinsics: Some(api::component::CameraIntrinsics {
            fx: 500.0,
            fy: 501.0,
            cx: 320.0,
            cy: 240.0,
        }),
        distortion: Some(api::component::CameraDistortion {
            model: "plumb_bob".to_string(),
            coefficients: vec![0.1, 0.2, 0.3],
        }),
        exposure: Some(api::component::CameraExposureTiming {
            exposure_start_ns: Some(100),
            exposure_duration_ns: Some(200),
        }),
        measured_at_ns: Some(300),
        calibration: Some(api::component::CameraCalibrationIdentity {
            id: "front".to_string(),
            version: "v1".to_string(),
        }),
        data: vec![1, 2, 3, 4],
    };
    let bytes = rmp_serde::to_vec_named(&frame).unwrap();
    let decoded: api::component::CameraFrame = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn topic_builder_keys_match_contract_topics() {
    assert_eq!(api::topic::new().drive().state().key(), "drive/state");
    assert_eq!(api::topic::new().drive().target().key(), "drive/target");
    assert_eq!(api::topic::new().battery().state().key(), "battery/state");
    assert_eq!(api::topic::new().safety().state().key(), "safety/state");
    assert_eq!(api::topic::new().motor().command().key(), "motor/command");
    assert_eq!(
        api::topic::new().presence().heartbeat().key(),
        "presence/heartbeat"
    );
}

// ---- dynamic / parameterized topics ----------------------------------------

#[test]
fn dynamic_topic_builder_fills_the_key_template() {
    let topic = api::topic::new()
        .component()
        .motor_command("front_left_drive", "motor");
    assert_eq!(
        topic.key(),
        "component/front_left_drive/motor/motor/command"
    );
    let enc = api::topic::new()
        .component()
        .encoder_sample("front_left_drive", "encoder");
    assert_eq!(
        enc.key(),
        "component/front_left_drive/encoder/encoder/sample"
    );
}

#[test]
fn recovered_component_capability_topic_builders_fill_key_templates() {
    let component = api::topic::new().component();
    assert_eq!(
        component.accelerometer_sample("imu0", "accel").key(),
        "component/imu0/accelerometer/accel/sample"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .gyroscope_sample("imu0", "gyro")
            .key(),
        "component/imu0/gyroscope/gyro/sample"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .magnetometer_sample("imu0", "mag")
            .key(),
        "component/imu0/magnetometer/mag/sample"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .imu_sample("imu0", "imu")
            .key(),
        "component/imu0/imu/imu/sample"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .range_sample("base", "front_tof")
            .key(),
        "component/base/range/front_tof/sample"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .gnss_sample("gps", "gnss")
            .key(),
        "component/gps/gnss/gnss/sample"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .camera_frame("head", "front")
            .key(),
        "component/head/camera/front/frame"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .depth_frame("head", "front_depth")
            .key(),
        "component/head/depth/front_depth/frame"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .lidar_scan("front_lidar", "scan")
            .key(),
        "component/front_lidar/lidar/scan/scan"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .mmwave_scan("radar", "mmwave")
            .key(),
        "component/radar/mmwave/mmwave/scan"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .microphone_frame("head", "mic")
            .key(),
        "component/head/microphone/mic/frame"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .led_command("status_panel", "status")
            .key(),
        "component/status_panel/led/status/command"
    );
    assert_eq!(
        api::topic::new()
            .component()
            .emergency_stop_state("safety_panel", "estop")
            .key(),
        "component/safety_panel/emergency_stop/estop/state"
    );
}

#[test]
fn dynamic_topic_contract_body_topic_is_the_template() {
    assert_eq!(
        <api::component::MotorCommand as ContractBody>::TOPIC,
        "component/{instance}/motor/{capability}/command"
    );
    assert_eq!(
        <api::component::MotorCommand as ContractBody>::FAMILY,
        "component::MotorCommand"
    );
    assert_eq!(
        <api::component::ImuSample as ContractBody>::TOPIC,
        "component/{instance}/imu/{capability}/sample"
    );
    assert_eq!(
        <api::component::ImuSample as ContractBody>::FAMILY,
        "component::ImuSample"
    );
}

#[test]
fn dynamic_topic_wildcard_is_subscribe_only() {
    let concrete = api::topic::new().component().motor_command("base", "motor");
    assert!(concrete.publish_key().is_ok());

    let wildcard = api::topic::new().component().motor_command("*", "motor");
    assert_eq!(wildcard.key(), "component/*/motor/motor/command");
    assert!(wildcard.publish_key().is_err());
}

// A self-contained three-version tree exercising multi-level inheritance
// (`vc extends vb extends va`) without touching the production api tree.
mod multi_level {
    use crate::api::{ApiVersion, ContractBody};

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
