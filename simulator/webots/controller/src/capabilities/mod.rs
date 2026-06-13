use anyhow::Result;

pub mod accelerometer;
pub mod battery;
pub mod camera;
pub mod depth;
pub mod downsample;
pub mod encoder;
pub mod gnss;
pub mod gyroscope;
pub mod imu;
pub mod led;
pub mod lidar;
pub mod magnetometer;
pub mod microphone;
pub mod mmwave;
pub mod motor;
pub mod range;
pub mod speaker;

pub(crate) fn sample_period_ms(rate_hz: f64) -> Result<i32> {
    if !rate_hz.is_finite() || rate_hz <= f64::EPSILON {
        anyhow::bail!("rate_hz must be > 0");
    }

    Ok((1000.0 / rate_hz).round() as i32)
}

pub(crate) fn publish_every_steps(basic_time_step_ms: i32, publish_rate_hz: f64) -> Result<u64> {
    if basic_time_step_ms <= 0 {
        anyhow::bail!("basic_time_step_ms must be > 0");
    }
    if !publish_rate_hz.is_finite() || publish_rate_hz <= f64::EPSILON {
        anyhow::bail!("publish_rate_hz must be > 0");
    }

    let publish_period_ms = 1000.0 / publish_rate_hz;
    Ok((publish_period_ms / f64::from(basic_time_step_ms))
        .round()
        .max(1.0) as u64)
}
