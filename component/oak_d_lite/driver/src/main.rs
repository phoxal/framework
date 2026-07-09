//! `oak_d_lite` - OAK-D Lite camera/depth/IMU component driver stub.

use anyhow::{Result, bail};
use phoxal::model::component::v0::capability::{CameraMode, Capability};
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

const STEP_HZ: f64 = 100.0;

#[derive(phoxal::Driver)]
#[phoxal(id = "oak_d_lite", api = y2026_1)]
struct OakDLite {
    camera: Vec<Publisher<api::component::camera::Frame>>,
    camera_specs: Vec<CameraSpec>,
    depth: Vec<Publisher<api::component::depth::Frame>>,
    depth_specs: Vec<DepthSpec>,
    imu: Vec<Publisher<api::component::imu::Sample>>,
    imu_divisors: Vec<u64>,
    accelerometer: Vec<Publisher<api::component::accelerometer::Sample>>,
    accelerometer_divisors: Vec<u64>,
    gyroscope: Vec<Publisher<api::component::gyroscope::Sample>>,
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
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
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
                ctx.publisher(
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
                ctx.publisher(
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
                ctx.publisher(
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
                ctx.publisher(
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
                ctx.publisher(
                    api::topic::internal::new(cap)
                        .component(&instance)
                        .gyroscope(&slot.capability_id)
                        .sample(),
                )
                .await?,
            );
            gyroscope_divisors.push(slot.divisor);
        }

        Ok(Self {
            camera,
            camera_specs,
            depth,
            depth_specs,
            imu,
            imu_divisors,
            accelerometer,
            accelerometer_divisors,
            gyroscope,
            gyroscope_divisors,
        })
    }

    #[step(hz = 100)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let at = step.time();
        let step_index = step.step_index();

        for (publisher, spec) in self.camera.iter().zip(&self.camera_specs) {
            if is_due(step_index, spec.divisor) {
                publisher.publish_at(at, camera_frame(spec)).await?;
            }
        }

        for (publisher, spec) in self.depth.iter().zip(&self.depth_specs) {
            if is_due(step_index, spec.divisor) {
                publisher.publish_at(at, depth_frame(spec)).await?;
            }
        }

        for (publisher, divisor) in self.imu.iter().zip(&self.imu_divisors) {
            if is_due(step_index, *divisor) {
                publisher.publish_at(at, imu_sample()).await?;
            }
        }

        for (publisher, divisor) in self.accelerometer.iter().zip(&self.accelerometer_divisors) {
            if is_due(step_index, *divisor) {
                publisher.publish_at(at, accelerometer_sample()).await?;
            }
        }

        for (publisher, divisor) in self.gyroscope.iter().zip(&self.gyroscope_divisors) {
            if is_due(step_index, *divisor) {
                publisher.publish_at(at, gyroscope_sample()).await?;
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
        measured_at_ns: None,
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
        measured_at_ns: None,
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
        measured_at_ns: None,
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

fn main() -> phoxal::Result<()> {
    phoxal::run::<OakDLite>()
}

#[cfg(test)]
mod tests {
    use super::{
        OakDLite, channels_for_encoding, divisor_for_rate, encoding_for_mode, frame_byte_len,
    };
    use phoxal::model::component::v0::capability::CameraMode;
    use phoxal_api::ContractBody;
    use phoxal_api::y2026_1 as api;

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
    fn emit_apis_reports_per_component_contracts() {
        let json = phoxal::participant::emit_apis_json::<OakDLite>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "oak_d_lite");
        assert_eq!(value["artifact"]["kind"], "driver");
        assert_eq!(value["participant_class"], "checked");
        let contracts = value["required_contracts"].as_array().unwrap();
        assert!(
            contracts
                .iter()
                .all(|c| c["topic"].as_str().is_some_and(|topic| !topic.is_empty())),
            "each contract should include topic"
        );
        for family in [
            <api::component::camera::Frame as ContractBody>::TOPIC,
            <api::component::depth::Frame as ContractBody>::TOPIC,
            <api::component::imu::Sample as ContractBody>::TOPIC,
            <api::component::accelerometer::Sample as ContractBody>::TOPIC,
            <api::component::gyroscope::Sample as ContractBody>::TOPIC,
        ] {
            assert!(
                contracts.iter().any(|c| c["topic"] == family),
                "missing contract for {family}"
            );
        }
    }
}
