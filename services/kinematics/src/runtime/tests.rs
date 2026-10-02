use phoxal::runtime::{ExecutionDuration, ExecutionTime, ObservationStamp, StepContext, invoke};

use super::phoxal_runtime_kinematics;
use super::*;
use crate::contract::kinematics_api::Inputs as ServiceInputs;

fn config() -> KinematicsConfig {
    KinematicsConfig::default()
}

fn new_service(config: KinematicsConfig) -> Kinematics {
    Kinematics::new(config).expect("valid configuration")
}

fn step(
    service: Kinematics,
    context: &StepContext,
    inputs: &ServiceInputs,
) -> (Kinematics, crate::contract::kinematics_api::Outputs) {
    invoke(
        &phoxal_runtime_kinematics::Adapter::new(),
        context,
        service,
        inputs,
    )
    .expect("the kinematics step accepts the inputs")
}

fn sample(
    encoder_id: &str,
    position_rad: f64,
    velocity_radps: f64,
    at_ms: u64,
) -> Sample<EncoderSample> {
    Sample::new(
        EncoderSample {
            position_rad: Some(position_rad),
            velocity_radps: Some(velocity_radps),
        },
        ObservationStamp::new(
            encoder_id,
            ExecutionTime::from_nanos(at_ms * 1_000_000),
            None,
        ),
    )
}

fn context(index: u64, now_ms: u64, previous_ms: Option<u64>) -> StepContext {
    StepContext::from_previous(
        ExecutionTime::from_nanos(now_ms * 1_000_000),
        ExecutionDuration::from_millis(20),
        previous_ms.map(|at| ExecutionTime::from_nanos(at * 1_000_000)),
        0,
        index,
    )
}

#[test]
fn one_encoder_batch_produces_joints_odometry_and_frames() {
    let service = new_service(config());
    let inputs = ServiceInputs {
        encoders: Samples::new(vec![
            sample("left_encoder", 2.0, 4.0, 20),
            sample("right_encoder", 2.0, 4.0, 20),
        ]),
        lookup_frame: Default::default(),
    };
    let (service, outputs) = step(service, &context(0, 20, None), &inputs);
    assert_eq!(outputs.joints.len(), 2);
    assert!(
        outputs
            .joints
            .iter()
            .any(|joint| joint.joint_id == "left_wheel" && joint.position_rad == 2.0)
    );
    assert!(service.state.available);
    assert_eq!(service.state.revision, 1);
    assert!(validation::frame_tree(&service.state.frames()).is_ok());
}

#[test]
fn expired_wheel_is_explicitly_unavailable_and_does_not_integrate() {
    let service = new_service(config());
    let (service, _) = step(
        service,
        &context(0, 20, None),
        &ServiceInputs {
            encoders: Samples::new(vec![
                sample("left_encoder", 0.0, 4.0, 20),
                sample("right_encoder", 0.0, 4.0, 20),
            ]),
            lookup_frame: Default::default(),
        },
    );
    let (service, _) = step(
        service,
        &context(1, 140, Some(20)),
        &ServiceInputs {
            encoders: Samples::new(vec![sample("left_encoder", 0.0, 4.0, 140)]),
            lookup_frame: Default::default(),
        },
    );
    assert!(!service.state.available);
    assert_eq!(service.state.linear_x_mps, 0.0);
    assert_eq!(service.state.angular_z_radps, 0.0);
    assert_eq!(service.state.revision, 1);
}

#[test]
fn stale_measurements_are_not_reused_as_fresh_motion() {
    let service = new_service(config());
    let (service, outputs) = step(
        service,
        &context(0, 200, None),
        &ServiceInputs {
            encoders: Samples::new(vec![
                sample("left_encoder", 0.0, 4.0, 20),
                sample("right_encoder", 0.0, 4.0, 20),
            ]),
            lookup_frame: Default::default(),
        },
    );
    assert!(outputs.joints.is_empty());
    assert!(!service.state.available);
    assert_eq!(service.state.linear_x_mps, 0.0);
}

#[test]
fn four_wheels_use_every_calibration_and_preserve_the_oldest_capture() {
    let mut cfg = config();
    cfg.left_wheels.push(crate::config::WheelEncoder {
        encoder_id: "left_rear".into(),
        joint_id: "left_rear_wheel".into(),
        longitudinal_offset_m: -0.18,
        direction_sign: -1,
        gear_ratio: 2.0,
    });
    cfg.right_wheels.push(crate::config::WheelEncoder {
        encoder_id: "right_rear".into(),
        joint_id: "right_rear_wheel".into(),
        longitudinal_offset_m: -0.18,
        direction_sign: -1,
        gear_ratio: 2.0,
    });
    validate_config(&cfg).unwrap();
    let service = new_service(cfg);
    let (service, outputs) = step(
        service,
        &context(0, 20, Some(0)),
        &ServiceInputs {
            encoders: Samples::new(vec![
                sample("left_encoder", 0.0, 2.0, 20),
                sample("left_rear", 0.0, -8.0, 10),
                sample("right_encoder", 0.0, 4.0, 20),
                sample("right_rear", 0.0, -12.0, 20),
            ]),
            lookup_frame: Default::default(),
        },
    );
    assert!(service.state.available);
    assert_eq!(outputs.joints.len(), 4);
    assert!((service.state.linear_x_mps - 0.4).abs() < 1e-12);
    assert!((service.state.angular_z_radps - 0.5).abs() < 1e-12);
    assert_eq!(service.state.oldest_capture_time_nanos, Some(10_000_000));
    // Exact constant-twist integration over 20 ms: radius .8 m, angle .01 rad.
    assert!((service.state.x_m - 0.8 * 0.01_f64.sin()).abs() < 1e-12);
    assert!((service.state.y_m - 0.8 * (1.0 - 0.01_f64.cos())).abs() < 1e-12);
    assert_eq!(service.state.frames().transforms.len(), 5);
    let rear = service
        .state
        .frames()
        .transforms
        .into_iter()
        .find(|frame| frame.child_frame_id == "left_rear_wheel")
        .unwrap();
    assert_eq!((rear.x_m, rear.y_m), (-0.18, 0.2));
    let (service, outputs) = step(
        service,
        &context(1, 40, Some(20)),
        &ServiceInputs {
            encoders: Samples::default(),
            lookup_frame: Default::default(),
        },
    );
    assert!(
        service.state.available,
        "slower captures remain usable within their original age bound"
    );
    assert_eq!(service.state.oldest_capture_time_nanos, Some(10_000_000));
    assert!(
        outputs.joints.is_empty(),
        "retention does not republish measured samples"
    );
    let (service, _) = step(
        service,
        &context(2, 111, Some(40)),
        &ServiceInputs {
            encoders: Samples::default(),
            lookup_frame: Default::default(),
        },
    );
    assert!(
        !service.state.available,
        "the oldest rear wheel expires before the other three"
    );
}

#[test]
fn invalid_new_encoder_replaces_old_evidence_until_a_new_valid_capture() {
    let service = new_service(config());
    let (service, _) = step(
        service,
        &context(0, 20, None),
        &ServiceInputs {
            encoders: Samples::new(vec![
                sample("left_encoder", 0.0, 1.0, 20),
                sample("right_encoder", 0.0, 1.0, 20),
            ]),
            lookup_frame: Default::default(),
        },
    );
    let (service, _) = step(
        service,
        &context(1, 40, Some(20)),
        &ServiceInputs {
            encoders: Samples::new(vec![sample("left_encoder", 0.0, f64::NAN, 40)]),
            lookup_frame: Default::default(),
        },
    );
    assert!(!service.state.available);
    let (service, _) = step(
        service,
        &context(2, 60, Some(40)),
        &ServiceInputs {
            encoders: Samples::default(),
            lookup_frame: Default::default(),
        },
    );
    assert!(
        !service.state.available,
        "a quiet interval cannot revive the older valid sample"
    );
    let (service, _) = step(
        service,
        &context(3, 80, Some(60)),
        &ServiceInputs {
            encoders: Samples::new(vec![sample("left_encoder", 0.0, 1.0, 80)]),
            lookup_frame: Default::default(),
        },
    );
    assert!(service.state.available);
    assert_eq!(service.state.oldest_capture_time_nanos, Some(20_000_000));
}
