use std::collections::BTreeSet;

/// One calibrated motor shaft driving a wheel on a configured side.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct WheelActuator {
    /// Logical actuator identity, including its component capability.
    pub actuator_id: String,
    /// Motor shaft rotations per wheel rotation.
    #[serde(default = "unit_ratio")]
    pub gear_ratio: f64,
    /// Motor direction producing positive forward wheel travel.
    #[serde(default = "positive_direction")]
    pub direction_sign: i8,
}

/// Admitted robot-specific motion limits and complete actuator membership.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct MotionConfig {
    /// Maximum absolute forward body velocity in metres per second.
    pub max_linear_mps: f64,
    /// Maximum absolute yaw body velocity in radians per second.
    pub max_angular_radps: f64,
    /// Wheel rolling radius in metres.
    pub wheel_radius_m: f64,
    /// Distance between wheel contact lines in metres.
    pub wheel_base_m: f64,
    /// One to four motors on the left side, in publication order.
    pub left_wheels: Vec<WheelActuator>,
    /// One to four motors on the right side, in publication order.
    pub right_wheels: Vec<WheelActuator>,
}

const fn unit_ratio() -> f64 {
    1.0
}
const fn positive_direction() -> i8 {
    1
}

pub(super) fn validate_motion_config(config: &MotionConfig) -> phoxal::Result<()> {
    for (name, value) in [
        ("max_linear_mps", config.max_linear_mps),
        ("max_angular_radps", config.max_angular_radps),
        ("wheel_radius_m", config.wheel_radius_m),
        ("wheel_base_m", config.wheel_base_m),
    ] {
        if !value.is_finite() || value <= 0.0 {
            return Err(anyhow::anyhow!("{name} must be finite and positive"));
        }
    }
    let max_wheel_rate = (config.max_linear_mps
        + config.max_angular_radps * config.wheel_base_m / 2.0)
        / config.wheel_radius_m;
    let mut ids = BTreeSet::new();
    for wheels in [&config.left_wheels, &config.right_wheels] {
        if !(1..=4).contains(&wheels.len()) {
            return Err(anyhow::anyhow!("each drive side requires 1 to 4 wheels"));
        }
        for wheel in wheels {
            if wheel.actuator_id.is_empty()
                || wheel.actuator_id.len() > 64
                || !ids.insert(&wheel.actuator_id)
            {
                return Err(anyhow::anyhow!(
                    "actuator IDs must be distinct and contain 1 to 64 bytes"
                ));
            }
            if !matches!(wheel.direction_sign, -1 | 1)
                || !wheel.gear_ratio.is_finite()
                || wheel.gear_ratio <= 0.0
            {
                return Err(anyhow::anyhow!(
                    "wheel gearing must be finite and positive and direction must be -1 or 1"
                ));
            }
            if !(max_wheel_rate * wheel.gear_ratio).is_finite() {
                return Err(anyhow::anyhow!(
                    "motion limits overflow motor angular velocity"
                ));
            }
        }
    }
    Ok(())
}
