//! `oak_d_lite` - OAK-D Lite camera/depth/IMU component driver stub.

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::v0::capability::{CameraMode, Capability};
use phoxal::prelude::*;

const STEP_HZ: f64 = 100.0;

#[derive(phoxal::Api)]
pub struct Api {
    camera: Vec<MeasurementPublisher<api::component::camera::Frame>>,
    depth: Vec<MeasurementPublisher<api::component::depth::Frame>>,
    imu: Vec<MeasurementPublisher<api::component::imu::Sample>>,
    accelerometer: Vec<MeasurementPublisher<api::component::accelerometer::Sample>>,
    gyroscope: Vec<MeasurementPublisher<api::component::gyroscope::Sample>>,
}

#[phoxal::driver(id = "oak_d_lite", config = ())]
pub struct OakDLite {
    camera_specs: Vec<CameraSpec>,
    depth_specs: Vec<DepthSpec>,
    imu_divisors: Vec<u64>,
    accelerometer_divisors: Vec<u64>,
    gyroscope_divisors: Vec<u64>,
}

#[derive(Debug, Clone)]
struct CameraSlot {
    capability_id: String,
    spec: CameraSpec,
}

#[derive(Debug, Clone)]
struct CameraSpec {
    divisor: u64,
    width: u32,
    height: u32,
    encoding: api::component::camera::Encoding,
    data_len: usize,
}

#[derive(Debug, Clone)]
struct DepthSlot {
    capability_id: String,
    spec: DepthSpec,
}

#[derive(Debug, Clone)]
struct DepthSpec {
    divisor: u64,
    width: u32,
    height: u32,
    sample_len: usize,
}

#[derive(Debug, Clone)]
struct SensorSlot {
    capability_id: String,
    divisor: u64,
}

#[phoxal::behavior]
impl OakDLite {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the owner
        // (`internal`) topic builder requires. This driver OWNS its component node.
        let cap = ctx.owner_capability();
        let instance = ctx.component()?.to_string();
        let (camera_slots, depth_slots, imu_slots, accelerometer_slots, gyroscope_slots) = {
            let robot = ctx.robot()?;
            let spec = robot.component_for_instance(&instance)?;
            let mut camera = Vec::new();
            let mut depth = Vec::new();
            let mut imu = Vec::new();
            let mut accelerometer = Vec::new();
            let mut gyroscope = Vec::new();

            for (capability_id, capability) in &spec.capabilities {
                match capability {
                    Capability::Camera(config) => {
                        validate_publish_rate(capability_id, config.publish_rate_hz)?;
                        let encoding = encoding_for_mode(config.mode);
                        let data_len = frame_byte_len(
                            config.width_px,
                            config.height_px,
                            channels_for_encoding(encoding),
                        )
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "camera capability '{capability_id}' frame dimensions are too large"
                            )
                        })?;
                        camera.push(CameraSlot {
                            capability_id: capability_id.to_string(),
                            spec: CameraSpec {
                                divisor: divisor_for_rate(STEP_HZ, config.publish_rate_hz),
                                width: config.width_px,
                                height: config.height_px,
                                encoding,
                                data_len,
                            },
                        });
                    }
                    Capability::Depth(config) => {
                        validate_publish_rate(capability_id, config.publish_rate_hz)?;
                        let sample_len =
                            depth_sample_len(config.width_px, config.height_px).ok_or_else(
                                || {
                                    anyhow::anyhow!(
                                        "depth capability '{capability_id}' frame dimensions are too large"
                                    )
                                },
                            )?;
                        depth.push(DepthSlot {
                            capability_id: capability_id.to_string(),
                            spec: DepthSpec {
                                divisor: divisor_for_rate(STEP_HZ, config.publish_rate_hz),
                                width: config.width_px,
                                height: config.height_px,
                                sample_len,
                            },
                        });
                    }
                    Capability::Imu(config) => {
                        imu.push(sensor_slot(capability_id, config.publish_rate_hz)?);
                    }
                    Capability::Accelerometer(config) => {
                        accelerometer.push(sensor_slot(capability_id, config.publish_rate_hz)?);
                    }
                    Capability::Gyroscope(config) => {
                        gyroscope.push(sensor_slot(capability_id, config.publish_rate_hz)?);
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
                    api::topic::internal::new(cap)
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
                    api::topic::internal::new(cap)
                        .component(&instance)
                        .depth(&slot.capability_id)
                        .frame(),
                )
                .await?,
            );
            depth_specs.push(slot.spec);
        }

        let mut imu = Vec::new();
        let mut imu_divisors = Vec::new();
        for slot in imu_slots {
            imu.push(
                ctx.measurement_publisher(
                    api::topic::internal::new(cap)
                        .component(&instance)
                        .imu(&slot.capability_id)
                        .sample(),
                )
                .await?,
            );
            imu_divisors.push(slot.divisor);
        }

        let mut accelerometer = Vec::new();
        let mut accelerometer_divisors = Vec::new();
        for slot in accelerometer_slots {
            accelerometer.push(
                ctx.measurement_publisher(
                    api::topic::internal::new(cap)
                        .component(&instance)
                        .accelerometer(&slot.capability_id)
                        .sample(),
                )
                .await?,
            );
            accelerometer_divisors.push(slot.divisor);
        }

        let mut gyroscope = Vec::new();
        let mut gyroscope_divisors = Vec::new();
        for slot in gyroscope_slots {
            gyroscope.push(
                ctx.measurement_publisher(
                    api::topic::internal::new(cap)
                        .component(&instance)
                        .gyroscope(&slot.capability_id)
                        .sample(),
                )
                .await?,
            );
            gyroscope_divisors.push(slot.divisor);
        }

        Ok((
            Self {
                camera_specs,
                depth_specs,
                imu_divisors,
                accelerometer_divisors,
                gyroscope_divisors,
            },
            Self::Api {
                camera,
                depth,
                imu,
                accelerometer,
                gyroscope,
            },
        ))
    }

    #[step(hz = 100)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let at = step.now();
        let step_index = step.step_index();

        for (publisher, spec) in api.camera.iter().zip(&self.camera_specs) {
            if is_due(step_index, spec.divisor) {
                publisher.publish(CaptureStamp::exact(at), camera_frame(spec))?;
            }
        }

        for (publisher, spec) in api.depth.iter().zip(&self.depth_specs) {
            if is_due(step_index, spec.divisor) {
                publisher.publish(CaptureStamp::exact(at), depth_frame(spec))?;
            }
        }

        for (publisher, divisor) in api.imu.iter().zip(&self.imu_divisors) {
            if is_due(step_index, *divisor) {
                publisher.publish(CaptureStamp::exact(at), imu_sample())?;
            }
        }

        for (publisher, divisor) in api.accelerometer.iter().zip(&self.accelerometer_divisors) {
            if is_due(step_index, *divisor) {
                publisher.publish(CaptureStamp::exact(at), accelerometer_sample())?;
            }
        }

        for (publisher, divisor) in api.gyroscope.iter().zip(&self.gyroscope_divisors) {
            if is_due(step_index, *divisor) {
                publisher.publish(CaptureStamp::exact(at), gyroscope_sample())?;
            }
        }

        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _ctx: ShutdownContext) -> Result<()> {
        Ok(())
    }
}

fn sensor_slot(capability_id: &str, publish_rate_hz: f64) -> Result<SensorSlot> {
    validate_publish_rate(capability_id, publish_rate_hz)?;
    Ok(SensorSlot {
        capability_id: capability_id.to_string(),
        divisor: divisor_for_rate(STEP_HZ, publish_rate_hz),
    })
}

fn validate_publish_rate(capability_id: &str, publish_rate_hz: f64) -> Result<()> {
    if !publish_rate_hz.is_finite() || publish_rate_hz <= 0.0 {
        bail!("capability '{capability_id}' publish_rate_hz must be > 0");
    }
    Ok(())
}

fn divisor_for_rate(step_hz: f64, publish_rate_hz: f64) -> u64 {
    (step_hz / publish_rate_hz).round().max(1.0) as u64
}

fn is_due(step_index: u64, divisor: u64) -> bool {
    divisor <= 1 || step_index % divisor == 0
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

fn frame_byte_len(width: u32, height: u32, channels: usize) -> Option<usize> {
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    let channels = u64::try_from(channels).ok()?;
    let bytes = pixels.checked_mul(channels)?;
    usize::try_from(bytes).ok()
}

fn depth_sample_len(width: u32, height: u32) -> Option<usize> {
    frame_byte_len(width, height, 1)
}

fn camera_frame(spec: &CameraSpec) -> api::component::camera::Frame {
    api::component::camera::Frame {
        width: spec.width,
        height: spec.height,
        encoding: spec.encoding,
        intrinsics: None,
        distortion: None,
        exposure: None,
        calibration: None,
        data: vec![0u8; spec.data_len],
    }
}

fn depth_frame(spec: &DepthSpec) -> api::component::depth::Frame {
    api::component::depth::Frame {
        samples_mm: vec![0u16; spec.sample_len],
        encoding: api::component::depth::Encoding::U16Millimeters,
        invalid_sample_policy: api::component::depth::InvalidSamplePolicy::ZeroIsInvalid,
        width: Some(spec.width),
        height: Some(spec.height),
        intrinsics: None,
        distortion: None,
        exposure: None,
        calibration: None,
    }
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
    use super::{
        OakDLite, channels_for_encoding, divisor_for_rate, encoding_for_mode, frame_byte_len,
    };
    use phoxal::api;
    use phoxal::bus::ContractBody;
    use phoxal::model::component::v0::capability::CameraMode;
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

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
        assert_eq!(divisor_for_rate(100.0, 25.0), 4);
    }

    #[test]
    fn api_reports_per_component_contracts() {
        assert_eq!(<OakDLite as Participant>::ID, "oak_d_lite");

        let contracts = <<OakDLite as Participant>::Api as ParticipantApi>::CONTRACTS;
        for family in [
            <api::component::camera::Frame as ContractBody>::TOPIC,
            <api::component::depth::Frame as ContractBody>::TOPIC,
            <api::component::imu::Sample as ContractBody>::TOPIC,
            <api::component::accelerometer::Sample as ContractBody>::TOPIC,
            <api::component::gyroscope::Sample as ContractBody>::TOPIC,
        ] {
            assert!(
                contracts
                    .iter()
                    .any(|c| { c.topic == family && c.role == ContractRole::Publish }),
                "missing Publish contract for {family}"
            );
        }
    }
}
