//! One module per component-capability family.
//!
//! Each module holds that family's spec, read from the robot's component
//! catalog, and the `NativeXxx` Webots device wrapper that reads or drives it.
//! Sensor families implement [`SimulatedSensor`], which owns the single rule
//! they share: a device is read only on the steps its publish cadence is due
//! on, and a step that produced no observation says so rather than repeating
//! the last one.

pub(crate) mod accelerometer;
pub(crate) mod battery;
pub(crate) mod camera;
pub(crate) mod depth;
pub(crate) mod encoder;
pub(crate) mod gnss;
pub(crate) mod gyroscope;
pub(crate) mod imu;
pub(crate) mod led;
pub(crate) mod lidar;
pub(crate) mod magnetometer;
pub(crate) mod microphone;
pub(crate) mod mmwave;
pub(crate) mod motor;
pub(crate) mod range;
pub(crate) mod speaker;

use anyhow::{Result, bail};
use phoxal::SampleSchedule;
use phoxal::model::identity::CapabilityRef;
use phoxal::model::simulation;

/// The world instant one completed step advanced to, and which step it was.
///
/// Most devices only need the index, to decide whether this step publishes.
/// The encoder and the battery also need the instant: both differentiate their
/// reading over the elapsed window to report a rate.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SensorStep {
    pub(crate) index: u64,
    pub(crate) time_ns: u64,
}

/// A Webots device the controller reads once per world step.
///
/// The cadence rule lives here rather than in each device: a capability
/// declares a publish rate, the controller resolves it to a step divisor once
/// at setup, and every family applies it the same way.
pub(crate) trait SimulatedSensor {
    /// The contract body this device produces.
    type Sample;

    /// The publish cadence this capability was bound at.
    fn schedule(&self) -> SampleSchedule;

    /// Read the device for `step`.
    ///
    /// `Ok(None)` means the device made no observation in this window. It is
    /// never a fabricated one: a sensor that saw nothing publishes nothing.
    fn read(&mut self, step: SensorStep) -> Result<Option<Self::Sample>>;

    /// The reading for `step`, or `None` when `step` is not a publish step.
    fn read_if_due(&mut self, step: SensorStep) -> Result<Option<Self::Sample>> {
        if !self.schedule().is_due(step.index) {
            return Ok(None);
        }
        self.read(step)
    }
}

/// A capability the world samples at a configured rate and the controller
/// publishes on the steps that rate is due on.
#[derive(Clone, Debug)]
pub(crate) struct SampledSpec {
    pub(crate) reference: CapabilityRef,
    pub(crate) schedule: SampleSchedule,
    /// The refresh period the Webots device is enabled with, in milliseconds.
    /// A world may model a device that samples faster than the component
    /// publishes, so this is resolved separately from the publish cadence.
    pub(crate) sampling_period_ms: i32,
}

impl SampledSpec {
    /// Resolve one sampled capability's publish cadence and device refresh
    /// period.
    ///
    /// `simulated` is the world's entry for the same capability, when the
    /// component type authored one. It decides how fast the device samples;
    /// with no entry the device samples at the rate the component publishes.
    ///
    /// # Errors
    ///
    /// Returns an error when the declared publish or sampling rate is not a
    /// positive finite number, which describes no cadence at all.
    pub(crate) fn new(
        reference: CapabilityRef,
        step_hz: f64,
        publish_rate_hz: f64,
        simulated: Option<&simulation::Capability>,
    ) -> Result<Self> {
        let sampling_hz = simulated
            .and_then(Self::simulated_sampling_hz)
            .unwrap_or(publish_rate_hz);
        let schedule = SampleSchedule::new(&reference.to_string(), step_hz, publish_rate_hz)?;
        Ok(Self {
            reference,
            schedule,
            sampling_period_ms: Self::sampling_period_ms(sampling_hz)?,
        })
    }

    /// The rate the world samples this capability at, when it models one.
    ///
    /// The actuator and event families carry no rate: nothing about a motor,
    /// speaker, battery, LED or emergency stop is sampled on a schedule the
    /// world sets.
    fn simulated_sampling_hz(capability: &simulation::Capability) -> Option<f64> {
        match capability {
            simulation::Capability::Encoder(config) => Some(config.sampling_period_hz),
            simulation::Capability::Accelerometer(config) => Some(config.sampling_period_hz),
            simulation::Capability::Gyroscope(config) => Some(config.sampling_period_hz),
            simulation::Capability::Magnetometer(config) => Some(config.sampling_period_hz),
            simulation::Capability::Imu(config) => Some(config.sampling_period_hz),
            simulation::Capability::Gnss(config) => Some(config.sampling_period_hz),
            simulation::Capability::Camera(config) => Some(config.sampling_period_hz),
            simulation::Capability::Depth(config) => Some(config.sampling_period_hz),
            simulation::Capability::Range(config) => Some(config.sampling_period_hz),
            simulation::Capability::Lidar(config) => Some(config.sampling_period_hz),
            simulation::Capability::Mmwave(config) => Some(config.sampling_period_hz),
            simulation::Capability::Microphone(config) => Some(config.sampling_period_hz),
            simulation::Capability::Motor(_)
            | simulation::Capability::EmergencyStop
            | simulation::Capability::Speaker
            | simulation::Capability::Battery
            | simulation::Capability::Led => None,
        }
    }

    /// The refresh period Webots is asked to sample the device at. Webots
    /// takes whole milliseconds and treats zero as "disabled", so the period
    /// is rounded and floored at one.
    fn sampling_period_ms(rate_hz: f64) -> Result<i32> {
        if !rate_hz.is_finite() || rate_hz <= 0.0 {
            bail!("sampling_period_hz must be finite and > 0");
        }
        Ok((1000.0 / rate_hz).round().max(1.0) as i32)
    }
}

/// Declare a three-axis Webots sensor that reports one `[f32; 3]` field.
///
/// Accelerometer, gyroscope and magnetometer are the same device wrapper three
/// times over: open the named device, enable it at the sampled period, and map
/// its three doubles into one contract field. Only the Webots accessor, the
/// device type and the body field differ, so they are named here rather than
/// copied.
macro_rules! vector_sensor {
    (
        $native:ident,
        $device:ty,
        $accessor:ident,
        $sample:ty,
        $field:ident $(,)?
    ) => {
        pub(crate) struct $native {
            device: $device,
            spec: $crate::capabilities::SampledSpec,
        }

        impl $native {
            pub(crate) fn new(
                webots: &webots_rs::Webots,
                spec: &$crate::capabilities::SampledSpec,
            ) -> ::anyhow::Result<Self> {
                let device = webots.$accessor(spec.reference.to_string())?;
                device.enable(spec.sampling_period_ms)?;
                Ok(Self {
                    device,
                    spec: spec.clone(),
                })
            }
        }

        impl $crate::capabilities::SimulatedSensor for $native {
            type Sample = $sample;

            fn schedule(&self) -> ::phoxal::SampleSchedule {
                self.spec.schedule
            }

            fn read(
                &mut self,
                _step: $crate::capabilities::SensorStep,
            ) -> ::anyhow::Result<Option<Self::Sample>> {
                // A captured type cannot be written directly in struct-literal
                // position, so the body type is named through a local alias.
                type Sample = $sample;
                Ok(Some(Sample {
                    $field: self.device.values()?.map(|value| value as f32),
                }))
            }
        }
    };
}

pub(crate) use vector_sensor;
