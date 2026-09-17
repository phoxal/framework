//! Bounded wheel evidence with source-owned capture time and calibrated SI values.
use crate::config::KinematicsConfig;
use crate::inputs::KinematicsInputs;
use phoxal::runtime::{ExecutionTime, ObservationStamp, Sample};
use phoxal_service_kinematics::{JointState, UnavailableReason};
use phoxal_robotics::EncoderSample;
use std::collections::{BTreeMap, BTreeSet};

pub(super) type RetainedEncoders = BTreeMap<String, (EncoderSample, ObservationStamp)>;

pub(super) struct WheelCut {
    pub joints: Vec<Sample<JointState>>,
    pub linear_mps: f64,
    pub angular_radps: f64,
    pub oldest_capture_time_nanos: u64,
}

pub(super) fn collect(
    config: &KinematicsConfig,
    retained: &mut RetainedEncoders,
    inputs: &KinematicsInputs,
    now: ExecutionTime,
) -> Result<WheelCut, UnavailableReason> {
    let mut updated = BTreeSet::new();
    for sample in inputs.encoders.items() {
        let id = sample.stamp().source();
        if !config
            .left_wheels
            .iter()
            .chain(&config.right_wheels)
            .any(|wheel| wheel.encoder_id == id)
        {
            return Err(UnavailableReason::InvalidMeasurement);
        }
        if sample.stamp().capture_time() > now {
            retained.remove(id);
            return Err(UnavailableReason::InvalidMeasurement);
        }
        if retained
            .get(id)
            .is_none_or(|(_, stamp)| stamp.capture_time() < sample.stamp().capture_time())
        {
            retained.insert(id.to_owned(), (*sample.payload(), sample.stamp().clone()));
            updated.insert(id);
        }
    }
    let mut sides = [0.0; 2];
    let mut oldest = now.as_nanos();
    let mut joints = Vec::new();
    for (index, wheels) in [&config.left_wheels, &config.right_wheels]
        .into_iter()
        .enumerate()
    {
        for wheel in wheels {
            let (measurement, stamp) = retained
                .get(&wheel.encoder_id)
                .ok_or(UnavailableReason::Encoder)?;
            let age = now
                .as_nanos()
                .checked_sub(stamp.capture_time().as_nanos())
                .ok_or(UnavailableReason::InvalidMeasurement)?;
            if age > config.max_age_ms.saturating_mul(1_000_000) {
                return Err(UnavailableReason::Encoder);
            }
            measurement
                .validate()
                .map_err(|_| UnavailableReason::InvalidMeasurement)?;
            let position = measurement
                .position_rad
                .ok_or(UnavailableReason::InvalidMeasurement)?;
            let velocity = measurement
                .velocity_radps
                .ok_or(UnavailableReason::InvalidMeasurement)?;
            let scale = f64::from(wheel.direction_sign) / wheel.gear_ratio;
            let joint = JointState {
                joint_id: wheel.joint_id.clone(),
                position_rad: position * scale,
                velocity_radps: velocity * scale,
                effort_nm: None,
            };
            joint
                .validate()
                .map_err(|_| UnavailableReason::InvalidMeasurement)?;
            // Every configured wheel contributes equally to its contact line.
            // Slip is not inferred or silently removed from this estimate.
            sides[index] += joint.velocity_radps * config.wheel_radius_m / wheels.len() as f64;
            oldest = oldest.min(stamp.capture_time().as_nanos());
            if updated.contains(wheel.encoder_id.as_str()) {
                joints.push(Sample::new(joint, stamp.clone()));
            }
        }
    }
    let linear = (sides[0] + sides[1]) / 2.0;
    let angular = (sides[1] - sides[0]) / config.wheel_base_m;
    if !linear.is_finite() || !angular.is_finite() {
        return Err(UnavailableReason::InvalidMeasurement);
    }
    Ok(WheelCut {
        joints,
        linear_mps: linear,
        angular_radps: angular,
        oldest_capture_time_nanos: oldest,
    })
}
