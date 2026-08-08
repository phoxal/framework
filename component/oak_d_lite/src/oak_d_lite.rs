//! `oak_d_lite` - OAK-D Lite camera/depth/IMU component driver stub.

use anyhow::{Result, anyhow, bail};
use phoxal::SampleSchedule;
use phoxal::api;
use phoxal::model::component::capability::{CameraMode, Capability};
use phoxal::model::identity::CapabilityId;
use phoxal::prelude::*;

/// The fixed step cadence every capability schedule below divides.
const STEP_HZ: f64 = 100.0;

pub(crate) struct Api {
    camera: Vec<MeasurementPublisher<api::component::camera::Frame>>,
    depth: Vec<MeasurementPublisher<api::component::depth::Frame>>,
    imu: Vec<MeasurementPublisher<api::component::imu::Sample>>,
    accelerometer: Vec<MeasurementPublisher<api::component::accelerometer::Sample>>,
    gyroscope: Vec<MeasurementPublisher<api::component::gyroscope::Sample>>,
}

pub(crate) struct OakDLiteState {
    camera_specs: Vec<CameraSpec>,
    depth_specs: Vec<DepthSpec>,
    imu_schedules: Vec<SampleSchedule>,
    accelerometer_schedules: Vec<SampleSchedule>,
    gyroscope_schedules: Vec<SampleSchedule>,
}

/// A declared capability paired with what drives its publisher. Publishers are
/// acquired in the order the slots are collected, so the spec or schedule at
/// index `i` belongs to the publisher at index `i`.
#[derive(Debug, Clone)]
struct CameraSlot {
    capability_id: CapabilityId,
    spec: CameraSpec,
}

#[derive(Debug, Clone)]
struct CameraSpec {
    schedule: SampleSchedule,
    width: u32,
    height: u32,
    encoding: api::component::camera::Encoding,
    data_len: usize,
}

impl CameraSpec {
    /// The frame this driver reports for the capability, sized from its
    /// declared geometry and encoding rather than read from a device.
    fn frame(&self) -> api::component::camera::Frame {
        api::component::camera::Frame {
            width: self.width,
            height: self.height,
            encoding: self.encoding,
            intrinsics: None,
            distortion: None,
            exposure: None,
            calibration: None,
            data: vec![0u8; self.data_len],
        }
    }
}

#[derive(Debug, Clone)]
struct DepthSlot {
    capability_id: CapabilityId,
    spec: DepthSpec,
}

#[derive(Debug, Clone)]
struct DepthSpec {
    schedule: SampleSchedule,
    width: u32,
    height: u32,
    sample_len: usize,
}

impl DepthSpec {
    /// The frame this driver reports for the capability, sized from its
    /// declared geometry rather than read from a device.
    fn frame(&self) -> api::component::depth::Frame {
        api::component::depth::Frame {
            samples_mm: vec![0u16; self.sample_len],
            encoding: api::component::depth::Encoding::U16Millimeters,
            invalid_sample_policy: api::component::depth::InvalidSamplePolicy::ZeroIsInvalid,
            width: Some(self.width),
            height: Some(self.height),
            intrinsics: None,
            distortion: None,
            exposure: None,
            calibration: None,
        }
    }
}

#[derive(Debug, Clone)]
struct SensorSlot {
    capability_id: CapabilityId,
    schedule: SampleSchedule,
}

impl SensorSlot {
    fn new(capability_id: &CapabilityId, publish_rate_hz: f64) -> Result<Self> {
        Ok(SensorSlot {
            capability_id: capability_id.clone(),
            schedule: SampleSchedule::new(capability_id.as_str(), STEP_HZ, publish_rate_hz)?,
        })
    }
}

#[phoxal::driver(state = OakDLiteState, api = Api)]
pub(crate) struct OakDLite;

impl Participant for OakDLite {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let instance = ctx.component()?.id().clone();
        let (camera_slots, depth_slots, imu_slots, accelerometer_slots, gyroscope_slots) = {
            let robot = ctx.robot()?;
            let spec = robot
                .component_for_instance(instance.as_str())
                .ok_or_else(|| anyhow!("no component type is loaded for instance '{instance}'"))?;
            let mut camera = Vec::new();
            let mut depth = Vec::new();
            let mut imu = Vec::new();
            let mut accelerometer = Vec::new();
            let mut gyroscope = Vec::new();

            for (capability_id, capability) in spec.capabilities() {
                match capability {
                    Capability::Camera(config) => {
                        // The cadence is resolved before the geometry so a
                        // capability that gets both wrong is reported by its
                        // rate first.
                        let schedule = SampleSchedule::new(
                            capability_id.as_str(),
                            STEP_HZ,
                            config.publish_rate_hz,
                        )?;
                        let encoding = encoding_for_mode(config.mode);
                        let data_len = frame_byte_len(
                            config.width_px,
                            config.height_px,
                            channels_for_encoding(encoding),
                        )
                        .ok_or_else(|| {
                            anyhow!(
                                "camera capability '{capability_id}' frame dimensions are too large"
                            )
                        })?;
                        camera.push(CameraSlot {
                            capability_id: capability_id.clone(),
                            spec: CameraSpec {
                                schedule,
                                width: config.width_px,
                                height: config.height_px,
                                encoding,
                                data_len,
                            },
                        });
                    }
                    Capability::Depth(config) => {
                        let schedule = SampleSchedule::new(
                            capability_id.as_str(),
                            STEP_HZ,
                            config.publish_rate_hz,
                        )?;
                        let sample_len = depth_sample_len(config.width_px, config.height_px)
                            .ok_or_else(|| {
                                anyhow!(
                                    "depth capability '{capability_id}' frame dimensions are too large"
                                )
                            })?;
                        depth.push(DepthSlot {
                            capability_id: capability_id.clone(),
                            spec: DepthSpec {
                                schedule,
                                width: config.width_px,
                                height: config.height_px,
                                sample_len,
                            },
                        });
                    }
                    Capability::Imu(config) => {
                        imu.push(SensorSlot::new(capability_id, config.publish_rate_hz)?);
                    }
                    Capability::Accelerometer(config) => {
                        accelerometer.push(SensorSlot::new(capability_id, config.publish_rate_hz)?);
                    }
                    Capability::Gyroscope(config) => {
                        gyroscope.push(SensorSlot::new(capability_id, config.publish_rate_hz)?);
                    }
                    _ => {}
                }
            }

            (camera, depth, imu, accelerometer, gyroscope)
        };

        if camera_slots.is_empty()
            && depth_slots.is_empty()
            && imu_slots.is_empty()
            && accelerometer_slots.is_empty()
            && gyroscope_slots.is_empty()
        {
            bail!(
                "oak_d_lite requires at least one camera, depth, imu, accelerometer, or gyroscope capability"
            );
        }

        let mut camera = Vec::new();
        let mut camera_specs = Vec::new();
        for slot in camera_slots {
            camera.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&instance)
                        .camera(&slot.capability_id)
                        .frame(),
                )
                .await?,
            );
            camera_specs.push(slot.spec);
        }

        let mut depth = Vec::new();
        let mut depth_specs = Vec::new();
        for slot in depth_slots {
            depth.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&instance)
                        .depth(&slot.capability_id)
                        .frame(),
                )
                .await?,
            );
            depth_specs.push(slot.spec);
        }

        let mut imu = Vec::new();
        let mut imu_schedules = Vec::new();
        for slot in imu_slots {
            imu.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&instance)
                        .imu(&slot.capability_id)
                        .sample(),
                )
                .await?,
            );
            imu_schedules.push(slot.schedule);
        }

        let mut accelerometer = Vec::new();
        let mut accelerometer_schedules = Vec::new();
        for slot in accelerometer_slots {
            accelerometer.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&instance)
                        .accelerometer(&slot.capability_id)
                        .sample(),
                )
                .await?,
            );
            accelerometer_schedules.push(slot.schedule);
        }

        let mut gyroscope = Vec::new();
        let mut gyroscope_schedules = Vec::new();
        for slot in gyroscope_slots {
            gyroscope.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&instance)
                        .gyroscope(&slot.capability_id)
                        .sample(),
                )
                .await?,
            );
            gyroscope_schedules.push(slot.schedule);
        }

        Ok((
            OakDLiteState {
                camera_specs,
                depth_specs,
                imu_schedules,
                accelerometer_schedules,
                gyroscope_schedules,
            },
            Api {
                camera,
                depth,
                imu,
                accelerometer,
                gyroscope,
            },
        ))
    }

    #[phoxal::step(hz = 100)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        let at = step.now();
        let step_index = step.step_index;

        for (publisher, spec) in api.camera.iter().zip(&state.camera_specs) {
            if spec.schedule.is_due(step_index) {
                publisher.publish(CaptureStamp::exact(at), spec.frame())?;
            }
        }

        for (publisher, spec) in api.depth.iter().zip(&state.depth_specs) {
            if spec.schedule.is_due(step_index) {
                publisher.publish(CaptureStamp::exact(at), spec.frame())?;
            }
        }

        for (publisher, schedule) in api.imu.iter().zip(&state.imu_schedules) {
            if schedule.is_due(step_index) {
                publisher.publish(CaptureStamp::exact(at), imu_sample())?;
            }
        }

        for (publisher, schedule) in api.accelerometer.iter().zip(&state.accelerometer_schedules) {
            if schedule.is_due(step_index) {
                publisher.publish(CaptureStamp::exact(at), accelerometer_sample())?;
            }
        }

        for (publisher, schedule) in api.gyroscope.iter().zip(&state.gyroscope_schedules) {
            if schedule.is_due(step_index) {
                publisher.publish(CaptureStamp::exact(at), gyroscope_sample())?;
            }
        }

        Ok(())
    }
}

fn encoding_for_mode(mode: CameraMode) -> api::component::camera::Encoding {
    match mode {
        CameraMode::Mono => api::component::camera::Encoding::L8,
        CameraMode::Rgb => api::component::camera::Encoding::Rgb8,
    }
}

fn channels_for_encoding(encoding: api::component::camera::Encoding) -> usize {
    match encoding {
        api::component::camera::Encoding::L8 => 1,
        api::component::camera::Encoding::Rgb8 => 3,
        api::component::camera::Encoding::Rgba8 => 4,
        api::component::camera::Encoding::Jpeg | api::component::camera::Encoding::Png => 1,
    }
}

/// The byte length of one uncompressed frame, or `None` when the declared
/// geometry does not fit an allocation on this host.
fn frame_byte_len(width: u32, height: u32, channels: usize) -> Option<usize> {
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    let channels = u64::try_from(channels).ok()?;
    let bytes = pixels.checked_mul(channels)?;
    usize::try_from(bytes).ok()
}

/// A depth frame carries exactly one `u16` sample per pixel.
fn depth_sample_len(width: u32, height: u32) -> Option<usize> {
    frame_byte_len(width, height, 1)
}

fn imu_sample() -> api::component::imu::Sample {
    api::component::imu::Sample {
        orientation: Some([1.0, 0.0, 0.0, 0.0]),
        angular_velocity_radps: [0.0; 3],
        linear_acceleration_mps2: [0.0; 3],
        covariance: None,
        noise_density: None,
        sensor_frame_id: None,
        health: api::component::imu::SensorHealth::Nominal,
        bias: None,
    }
}

fn accelerometer_sample() -> api::component::accelerometer::Sample {
    api::component::accelerometer::Sample {
        linear_acceleration: [0.0; 3],
    }
}

fn gyroscope_sample() -> api::component::gyroscope::Sample {
    api::component::gyroscope::Sample {
        angular_velocity: [0.0; 3],
    }
}

#[cfg(test)]
mod tests {
    use super::{channels_for_encoding, depth_sample_len, encoding_for_mode, frame_byte_len};
    use phoxal::api;
    use phoxal::model::component::capability::CameraMode;

    #[test]
    fn camera_encoding_and_frame_size_follow_mode() {
        assert!(matches!(
            encoding_for_mode(CameraMode::Mono),
            api::component::camera::Encoding::L8
        ));
        assert!(matches!(
            encoding_for_mode(CameraMode::Rgb),
            api::component::camera::Encoding::Rgb8
        ));
        assert_eq!(
            channels_for_encoding(api::component::camera::Encoding::L8),
            1
        );
        assert_eq!(
            channels_for_encoding(api::component::camera::Encoding::Rgb8),
            3
        );
        assert_eq!(frame_byte_len(640, 480, 3), Some(921_600));
    }

    #[test]
    fn a_frame_larger_than_this_host_can_address_is_rejected() {
        assert_eq!(frame_byte_len(u32::MAX, u32::MAX, 4), None);
        assert_eq!(depth_sample_len(640, 480), Some(307_200));
    }
}
