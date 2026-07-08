//! One module per component-capability family. Each module holds that
//! family's spec struct (parsed from the robot's component catalog) plus the
//! `NativeXxx` Webots device wrapper that reads/writes it. This mirrors the
//! pre-rewrite v0.10 two-binary layout (`simulator/webots/controller/src/capabilities/`)
//! structurally; the contracts and derive underneath are today's.

pub mod accelerometer;
pub mod camera;
pub mod depth;
pub mod encoder;
pub mod gnss;
pub mod gyroscope;
pub mod imu;
pub mod motor;
pub mod range;

use anyhow::{Result, bail};
use phoxal::model::component::v0::CapabilityRef;

/// A capability sampled at a configured rate and published only when its
/// downsample window is due (shared by IMU, accelerometer, gyroscope, range,
/// camera, depth, and GNSS).
#[derive(Clone, Debug)]
pub(crate) struct SampledSpec {
    pub(crate) reference: CapabilityRef,
    pub(crate) publish_every_steps: u64,
    pub(crate) sampling_period_ms: i32,
}

pub(crate) fn publish_every_steps(step_hz: f64, publish_rate_hz: f64) -> Result<u64> {
    if !step_hz.is_finite() || step_hz <= 0.0 {
        bail!("step_hz must be finite and > 0");
    }
    if !publish_rate_hz.is_finite() || publish_rate_hz <= 0.0 {
        bail!("publish_rate_hz must be finite and > 0");
    }
    Ok((step_hz / publish_rate_hz).round().max(1.0) as u64)
}

pub(crate) fn sampling_period_ms(rate_hz: f64) -> Result<i32> {
    if !rate_hz.is_finite() || rate_hz <= 0.0 {
        bail!("sampling_period_hz must be finite and > 0");
    }
    Ok((1000.0 / rate_hz).round().max(1.0) as i32)
}

pub(crate) fn is_due(step_index: u64, publish_every_steps: u64) -> bool {
    publish_every_steps <= 1 || step_index % publish_every_steps == 0
}
