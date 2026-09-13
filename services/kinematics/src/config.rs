use std::collections::BTreeSet;

/// One encoder and the planar center of its calibrated wheel.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct WheelEncoder {
    /// Source identity carried by the encoder sample.
    pub encoder_id: String,
    /// Child frame identity for the wheel center.
    pub joint_id: String,
    /// Forward offset of the wheel center from the body origin, in metres.
    #[serde(default)]
    pub longitudinal_offset_m: f64,
    /// Encoder direction producing forward rolling motion.
    #[serde(default = "positive_direction")]
    pub direction_sign: i8,
    /// Encoder shaft rotations per wheel rotation.
    #[serde(default = "unit_ratio")]
    pub gear_ratio: f64,
}

/// Calibrated differential or skid-steer odometry with explicit wheel membership.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct KinematicsConfig {
    /// One to four encoders on the left contact line.
    pub left_wheels: Vec<WheelEncoder>,
    /// One to four encoders on the right contact line.
    pub right_wheels: Vec<WheelEncoder>,
    /// Wheel rolling radius in metres.
    pub wheel_radius_m: f64,
    /// Distance between wheel contact lines in metres.
    pub wheel_base_m: f64,
    /// Frame containing the integrated planar pose.
    #[serde(default = "default_odom_frame_id")]
    pub odom_frame_id: String,
    /// Frame fixed to the robot body.
    #[serde(default = "default_base_frame_id")]
    pub base_frame_id: String,
    /// Maximum source capture age, never renewed by retention.
    #[serde(default = "default_max_age_ms")]
    pub max_age_ms: u64,
    /// One to 256 frame trees retained for bounded reads.
    #[serde(default = "default_history_capacity")]
    pub history_capacity: u32,
}

const fn positive_direction() -> i8 {
    1
}
const fn unit_ratio() -> f64 {
    1.0
}
fn default_odom_frame_id() -> String {
    "odom".into()
}
fn default_base_frame_id() -> String {
    "base_link".into()
}
const fn default_max_age_ms() -> u64 {
    100
}
const fn default_history_capacity() -> u32 {
    64
}

#[cfg(test)]
impl Default for KinematicsConfig {
    fn default() -> Self {
        let wheel = |side: &str| WheelEncoder {
            encoder_id: format!("{side}_encoder"),
            joint_id: format!("{side}_wheel"),
            longitudinal_offset_m: 0.0,
            direction_sign: 1,
            gear_ratio: 1.0,
        };
        Self {
            left_wheels: vec![wheel("left")],
            right_wheels: vec![wheel("right")],
            wheel_radius_m: 0.1,
            wheel_base_m: 0.4,
            odom_frame_id: default_odom_frame_id(),
            base_frame_id: default_base_frame_id(),
            max_age_ms: default_max_age_ms(),
            history_capacity: default_history_capacity(),
        }
    }
}

pub(super) fn validate_config(config: &KinematicsConfig) -> phoxal::Result<()> {
    let mut ids = BTreeSet::new();
    let mut id = |value: &String| -> phoxal::Result<()> {
        if value.is_empty()
            || value.len() > phoxal_kinematics::MAX_ID_BYTES
            || !ids.insert(value.clone())
        {
            return Err(anyhow::anyhow!(
                "encoder, joint, and frame IDs must be distinct and contain 1 to {} bytes",
                phoxal_kinematics::MAX_ID_BYTES
            ));
        }
        Ok(())
    };
    id(&config.odom_frame_id)?;
    id(&config.base_frame_id)?;
    for wheels in [&config.left_wheels, &config.right_wheels] {
        if !(1..=4).contains(&wheels.len()) {
            return Err(anyhow::anyhow!("each drive side requires 1 to 4 encoders"));
        }
        for wheel in wheels {
            id(&wheel.encoder_id)?;
            id(&wheel.joint_id)?;
            if !matches!(wheel.direction_sign, -1 | 1)
                || !wheel.gear_ratio.is_finite()
                || wheel.gear_ratio <= 0.0
                || !(1.0 / wheel.gear_ratio).is_finite()
                || !wheel.longitudinal_offset_m.is_finite()
            {
                return Err(anyhow::anyhow!(
                    "wheel calibration needs finite positive gearing, a finite offset, and direction -1 or 1"
                ));
            }
        }
    }
    for value in [config.wheel_radius_m, config.wheel_base_m] {
        if !value.is_finite() || value <= 0.0 {
            return Err(anyhow::anyhow!(
                "wheel radius and base must be finite and positive"
            ));
        }
    }
    if config.max_age_ms == 0 || !(1..=256).contains(&config.history_capacity) {
        return Err(anyhow::anyhow!(
            "capture age must be positive and frame history must contain 1 to 256 entries"
        ));
    }
    Ok(())
}
