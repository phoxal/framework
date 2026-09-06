//! Typed sampled Webots devices bound one-to-one to framework publishers.

use anyhow::{Context, Result, bail};
use phoxal::SampleSchedule;
use phoxal::api;
use phoxal::model::component::capability::{
    CameraMode, Capability as DeclaredCapability, GnssCoordinateSystem,
};
use phoxal::simulator::{LiveSamplePublisher, LiveTransitionStamp, SimulatorSession};
use phoxal_simulator_webots_shared::plan::CapabilityBinding;
use webots_rs::Webots;

pub(crate) struct SensorSet {
    devices: Vec<SensorDevice>,
}

enum SensorDevice {
    Accelerometer(VectorSensor<webots_rs::device::accelerometer::Accelerometer>),
    Gyroscope(VectorSensor<webots_rs::device::gyro::Gyro>),
    Imu(ImuSensor),
    Camera(CameraSensor),
    Depth(DepthSensor),
    Gnss(GnssSensor),
    Range(RangeSensor),
}

struct VectorSensor<D> {
    device: D,
    axes: Option<[bool; 3]>,
    schedule: SampleSchedule,
    publisher: VectorPublisher,
}

enum VectorPublisher {
    Accelerometer(LiveSamplePublisher<api::component::accelerometer::Sample>),
    Gyroscope(LiveSamplePublisher<api::component::gyroscope::Sample>),
}

struct ImuSensor {
    inertial: webots_rs::device::inertial_unit::InertialUnit,
    accelerometer: webots_rs::device::accelerometer::Accelerometer,
    gyroscope: webots_rs::device::gyro::Gyro,
    axes: Option<[bool; 3]>,
    schedule: SampleSchedule,
    publisher: LiveSamplePublisher<api::component::imu::Sample>,
}

struct CameraSensor {
    device: webots_rs::device::camera::Camera,
    mode: CameraMode,
    width: u32,
    height: u32,
    schedule: SampleSchedule,
    publisher: LiveSamplePublisher<api::component::camera::Frame>,
}

struct DepthSensor {
    device: webots_rs::device::range_finder::RangeFinder,
    width: u32,
    height: u32,
    schedule: SampleSchedule,
    publisher: LiveSamplePublisher<api::component::depth::Frame>,
}

struct GnssSensor {
    device: webots_rs::device::gps::Gps,
    schedule: SampleSchedule,
    publisher: LiveSamplePublisher<api::component::gnss::Sample>,
}

struct RangeSensor {
    device: webots_rs::device::distance_sensor::DistanceSensor,
    min_m: f32,
    max_m: f32,
    schedule: SampleSchedule,
    publisher: LiveSamplePublisher<api::component::range::Sample>,
}

impl SensorSet {
    pub(crate) fn bind(
        session: &SimulatorSession,
        webots: &Webots,
        bindings: &[CapabilityBinding],
        source_start_ns: u64,
    ) -> Result<Self> {
        let mut devices = Vec::new();
        for binding in bindings {
            let declared = session
                .robot()
                .capability(&binding.reference)
                .with_context(|| format!("plan capability {} is absent", binding.reference))?;
            let component = || api::topics().component(&binding.reference.component_id);
            let id = &binding.reference.capability_id;
            let Some(sampling) = binding.sampling else {
                continue;
            };
            let schedule = schedule(binding, source_start_ns)?;
            let device = match declared {
                DeclaredCapability::Accelerometer(config) => {
                    let native = webots.accelerometer(&binding.native_device)?;
                    native.enable(sampling.native_period_ms)?;
                    SensorDevice::Accelerometer(VectorSensor {
                        device: native,
                        axes: config.axes,
                        schedule,
                        publisher: VectorPublisher::Accelerometer(
                            session.sample_publisher(
                                component()?.accelerometer(id)?.sample().owner(),
                            )?,
                        ),
                    })
                }
                DeclaredCapability::Gyroscope(config) => {
                    let native = webots.gyro(&binding.native_device)?;
                    native.enable(sampling.native_period_ms)?;
                    SensorDevice::Gyroscope(VectorSensor {
                        device: native,
                        axes: config.axes,
                        schedule,
                        publisher: VectorPublisher::Gyroscope(
                            session
                                .sample_publisher(component()?.gyroscope(id)?.sample().owner())?,
                        ),
                    })
                }
                DeclaredCapability::Imu(config) => {
                    let inertial = webots.inertial_unit(&binding.native_device)?;
                    let accelerometer =
                        webots.accelerometer(format!("{}__accel", binding.native_device))?;
                    let gyroscope = webots.gyro(format!("{}__gyro", binding.native_device))?;
                    inertial.enable(sampling.native_period_ms)?;
                    accelerometer.enable(sampling.native_period_ms)?;
                    gyroscope.enable(sampling.native_period_ms)?;
                    SensorDevice::Imu(ImuSensor {
                        inertial,
                        accelerometer,
                        gyroscope,
                        axes: config.axes,
                        schedule,
                        publisher: session
                            .sample_publisher(component()?.imu(id)?.sample().owner())?,
                    })
                }
                DeclaredCapability::Camera(config) => {
                    let native = webots.camera(&binding.native_device)?;
                    native.enable(sampling.native_period_ms)?;
                    SensorDevice::Camera(CameraSensor {
                        device: native,
                        mode: config.mode,
                        width: config.width_px,
                        height: config.height_px,
                        schedule,
                        publisher: session
                            .sample_publisher(component()?.camera(id)?.frame().owner())?,
                    })
                }
                DeclaredCapability::Depth(config) => {
                    let native = webots.range_finder(&binding.native_device)?;
                    native.enable(sampling.native_period_ms)?;
                    SensorDevice::Depth(DepthSensor {
                        device: native,
                        width: config.width_px,
                        height: config.height_px,
                        schedule,
                        publisher: session
                            .sample_publisher(component()?.depth(id)?.frame().owner())?,
                    })
                }
                DeclaredCapability::Gnss(config) => {
                    if config.coordinate_system != GnssCoordinateSystem::Wgs84 {
                        bail!("GNSS binding {} is not explicitly wgs84", binding.reference);
                    }
                    let native = webots.gps(&binding.native_device)?;
                    if native.get_coordinate_system()?
                        != webots_rs::device::gps::GpsCoordinateSystem::Wgs84
                    {
                        bail!(
                            "GNSS binding {} resolved a non-WGS84 native GPS",
                            binding.reference
                        );
                    }
                    native.enable(sampling.native_period_ms)?;
                    SensorDevice::Gnss(GnssSensor {
                        device: native,
                        schedule,
                        publisher: session
                            .sample_publisher(component()?.gnss(id)?.sample().owner())?,
                    })
                }
                DeclaredCapability::Range(config) => {
                    let native = webots.distance_sensor(&binding.native_device)?;
                    native.enable(sampling.native_period_ms)?;
                    SensorDevice::Range(RangeSensor {
                        device: native,
                        min_m: config.min_range_m as f32,
                        max_m: config.max_range_m as f32,
                        schedule,
                        publisher: session
                            .sample_publisher(component()?.range(id)?.sample().owner())?,
                    })
                }
                DeclaredCapability::Motor(_) | DeclaredCapability::Encoder(_) => continue,
                other => bail!(
                    "plan admitted unsupported sampled capability {} ({})",
                    binding.reference,
                    other.kind()
                ),
            };
            devices.push(device);
        }
        Ok(Self { devices })
    }

    pub(crate) fn publish_outputs(&mut self, transition: &LiveTransitionStamp) -> Result<()> {
        for device in &mut self.devices {
            device.publish_if_due(transition)?;
        }
        Ok(())
    }
}

impl SensorDevice {
    fn publish_if_due(&mut self, transition: &LiveTransitionStamp) -> Result<()> {
        let elapsed_ns = transition.progress().elapsed_ns();
        match self {
            Self::Accelerometer(sensor) => {
                if sensor.schedule.is_due_at(elapsed_ns)? {
                    let values = mask(
                        sensor.device.values()?.map(|value| value as f32),
                        sensor.axes,
                    );
                    let sample = api::component::accelerometer::Sample::try_new(values)?;
                    let VectorPublisher::Accelerometer(publisher) = &sensor.publisher else {
                        bail!("accelerometer publisher kind changed after binding");
                    };
                    publisher.publish(transition, sample)?;
                }
            }
            Self::Gyroscope(sensor) => {
                if sensor.schedule.is_due_at(elapsed_ns)? {
                    let values = mask(
                        sensor.device.values()?.map(|value| value as f32),
                        sensor.axes,
                    );
                    let sample = api::component::gyroscope::Sample::try_new(values)?;
                    let VectorPublisher::Gyroscope(publisher) = &sensor.publisher else {
                        bail!("gyroscope publisher kind changed after binding");
                    };
                    publisher.publish(transition, sample)?;
                }
            }
            Self::Imu(sensor) => {
                if sensor.schedule.is_due_at(elapsed_ns)? {
                    let [roll, pitch, yaw] = sensor.inertial.get_roll_pitch_yaw()?;
                    let acceleration = mask(
                        sensor.accelerometer.values()?.map(|value| value as f32),
                        sensor.axes,
                    );
                    let angular_velocity = mask(
                        sensor.gyroscope.values()?.map(|value| value as f32),
                        sensor.axes,
                    );
                    sensor.publisher.publish(
                        transition,
                        api::component::imu::Sample::try_new(
                            Some(quaternion_wxyz_from_rpy(roll, pitch, yaw)),
                            angular_velocity,
                            acceleration,
                            None,
                            None,
                            None,
                            api::component::imu::SensorHealth::Nominal,
                            None,
                        )?,
                    )?;
                }
            }
            Self::Camera(sensor) => {
                if sensor.schedule.is_due_at(elapsed_ns)? {
                    let bgra = sensor.device.get_image()?;
                    let (encoding, data) = match sensor.mode {
                        CameraMode::Mono => {
                            (api::component::camera::Encoding::L8, bgra_to_luma(&bgra))
                        }
                        CameraMode::Rgb => {
                            (api::component::camera::Encoding::Rgb8, bgra_to_rgb(&bgra))
                        }
                    };
                    sensor.publisher.publish(
                        transition,
                        api::component::camera::Frame::try_new(
                            sensor.width,
                            sensor.height,
                            encoding,
                            None,
                            None,
                            None,
                            None,
                            data,
                        )?,
                    )?;
                }
            }
            Self::Depth(sensor) => {
                if sensor.schedule.is_due_at(elapsed_ns)? {
                    let samples = sensor
                        .device
                        .get_range_image()?
                        .into_iter()
                        .map(meters_to_u16_mm)
                        .collect();
                    sensor.publisher.publish(
                        transition,
                        api::component::depth::Frame::try_new(
                            samples,
                            api::component::depth::Encoding::U16Millimeters,
                            api::component::depth::InvalidSamplePolicy::ZeroIsInvalid,
                            sensor.width,
                            sensor.height,
                            None,
                            None,
                            None,
                            None,
                        )?,
                    )?;
                }
            }
            Self::Gnss(sensor) => {
                if sensor.schedule.is_due_at(elapsed_ns)? {
                    let reading = sensor.device.reading()?;
                    sensor.publisher.publish(
                        transition,
                        api::component::gnss::Sample::try_new(
                            reading.position[0],
                            reading.position[1],
                            reading.position[2],
                            [0.0; 9],
                        )?,
                    )?;
                }
            }
            Self::Range(sensor) => {
                if sensor.schedule.is_due_at(elapsed_ns)? {
                    sensor.publisher.publish(
                        transition,
                        api::component::range::Sample::try_new(
                            sensor.device.value()? as f32,
                            Some(api::component::range::Limits {
                                min_m: sensor.min_m,
                                max_m: sensor.max_m,
                            }),
                            Some(api::component::range::SampleQuality {
                                valid: true,
                                confidence: None,
                            }),
                            api::component::range::SensorHealth::Nominal,
                        )?,
                    )?;
                }
            }
        }
        Ok(())
    }
}

pub(super) fn schedule(
    binding: &CapabilityBinding,
    source_start_ns: u64,
) -> Result<SampleSchedule> {
    let sampling = binding.sampling.context("sampled binding has no cadence")?;
    let source_period_ns = u64::try_from(sampling.native_period_ms)?
        .checked_mul(1_000_000)
        .context("native sampling period overflows nanoseconds")?;
    let mut schedule = SampleSchedule::from_source_period_ns(
        &binding.reference.to_string(),
        source_period_ns,
        sampling.publish_rate_hz,
    )?;
    schedule.reanchor_after(source_start_ns, source_period_ns)?;
    Ok(schedule)
}

fn mask(mut values: [f32; 3], axes: Option<[bool; 3]>) -> [f32; 3] {
    if let Some(axes) = axes {
        for (value, enabled) in values.iter_mut().zip(axes) {
            if !enabled {
                *value = 0.0;
            }
        }
    }
    values
}

fn quaternion_wxyz_from_rpy(roll: f64, pitch: f64, yaw: f64) -> [f32; 4] {
    let (sr, cr) = (roll * 0.5).sin_cos();
    let (sp, cp) = (pitch * 0.5).sin_cos();
    let (sy, cy) = (yaw * 0.5).sin_cos();
    [
        (cr * cp * cy + sr * sp * sy) as f32,
        (sr * cp * cy - cr * sp * sy) as f32,
        (cr * sp * cy + sr * cp * sy) as f32,
        (cr * cp * sy - sr * sp * cy) as f32,
    ]
}

fn bgra_to_rgb(bgra: &[u8]) -> Vec<u8> {
    bgra.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0]])
        .collect()
}

fn bgra_to_luma(bgra: &[u8]) -> Vec<u8> {
    bgra.as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| {
            let red = u32::from(pixel[2]);
            let green = u32::from(pixel[1]);
            let blue = u32::from(pixel[0]);
            ((299 * red + 587 * green + 114 * blue) / 1000) as u8
        })
        .collect()
}

fn meters_to_u16_mm(meters: f32) -> u16 {
    if !meters.is_finite() || meters <= 0.0 {
        return 0;
    }
    let millimeters = (meters * 1000.0).round();
    if !(1.0..=f32::from(u16::MAX)).contains(&millimeters) {
        return 0;
    }
    millimeters as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_are_deterministic_and_fail_closed() {
        assert_eq!(bgra_to_rgb(&[10, 20, 30, 255]), vec![30, 20, 10]);
        assert_eq!(bgra_to_luma(&[10, 20, 30, 255]), vec![21]);
        assert_eq!(meters_to_u16_mm(1.25), 1250);
        assert_eq!(meters_to_u16_mm(f32::NAN), 0);
    }

    #[test]
    fn quaternion_is_wxyz() {
        let quaternion = quaternion_wxyz_from_rpy(0.0, 0.0, std::f64::consts::FRAC_PI_2);
        assert!((f64::from(quaternion[0]) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((f64::from(quaternion[3]) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }

    #[test]
    fn every_sample_waits_for_a_native_observation_after_late_attachment() {
        use phoxal::model::identity::{CapabilityId, CapabilityRef, ComponentInstanceId};
        use phoxal_simulator_webots_shared::plan::{PlannedTarget, SamplingPlan};
        let binding = CapabilityBinding {
            reference: CapabilityRef {
                component_id: ComponentInstanceId::new("wheel").expect("component"),
                capability_id: CapabilityId::new("encoder").expect("capability"),
            },
            kind: "encoder".to_owned(),
            native_device: "wheel.encoder".to_owned(),
            target: PlannedTarget::Joint {
                id: "axle".to_owned(),
            },
            actuation: None,
            sampling: Some(SamplingPlan {
                publish_rate_hz: 50.0,
                native_sampling_rate_hz: 50.0,
                native_period_ms: 24,
                publish_period_ns: 20_000_000,
            }),
        };
        for start in [0, 12_000_000_000] {
            let mut schedule = schedule(&binding, start).expect("native schedule");
            assert!(
                !schedule
                    .is_due_at(start + 12_000_000)
                    .expect("first transition")
            );
            assert!(
                schedule
                    .is_due_at(start + 24_000_000)
                    .expect("first captured sample")
            );
            assert!(
                !schedule
                    .is_due_at(start + 36_000_000)
                    .expect("no duplicate capture")
            );
        }
    }
}
