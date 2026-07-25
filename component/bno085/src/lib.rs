//! `bno085` - BNO085 IMU component driver stub.

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::v0::capability::Capability;
use phoxal::prelude::*;

const STEP_HZ: f64 = 100.0;

#[derive(phoxal::Api)]
pub struct Api {
    imu: Vec<MeasurementPublisher<api::component::imu::Sample>>,
    accelerometer: Vec<MeasurementPublisher<api::component::accelerometer::Sample>>,
    gyroscope: Vec<MeasurementPublisher<api::component::gyroscope::Sample>>,
}

#[phoxal::driver(id = "bno085", config = ())]
pub struct Bno085 {
    imu_divisors: Vec<u64>,
    accelerometer_divisors: Vec<u64>,
    gyroscope_divisors: Vec<u64>,
}

#[derive(Debug, Clone)]
struct CapabilitySchedule {
    capability_id: String,
    divisor: u64,
}

#[phoxal::behavior]
impl Bno085 {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the owner
        // (`internal`) topic builder requires. This driver OWNS its component node.
        let cap = ctx.owner_capability();
        let instance = ctx.component()?.to_string();
        let (imu_slots, accelerometer_slots, gyroscope_slots) = {
            let robot = ctx.robot()?;
            let spec = robot.component_for_instance(&instance)?;
            let mut imu = Vec::new();
            let mut accelerometer = Vec::new();
            let mut gyroscope = Vec::new();

            for (capability_id, capability) in &spec.capabilities {
                match capability {
                    Capability::Imu(config) => {
                        imu.push(schedule(capability_id, config.publish_rate_hz)?);
                    }
                    Capability::Accelerometer(config) => {
                        accelerometer.push(schedule(capability_id, config.publish_rate_hz)?);
                    }
                    Capability::Gyroscope(config) => {
                        gyroscope.push(schedule(capability_id, config.publish_rate_hz)?);
                    }
                    _ => {}
                }
            }

            (imu, accelerometer, gyroscope)
        };

        if imu_slots.is_empty() {
            bail!("bno085 requires at least one imu capability");
        }
        if accelerometer_slots.is_empty() {
            bail!("bno085 requires at least one accelerometer capability");
        }
        if gyroscope_slots.is_empty() {
            bail!("bno085 requires at least one gyroscope capability");
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
                imu_divisors,
                accelerometer_divisors,
                gyroscope_divisors,
            },
            Self::Api {
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

fn schedule(capability_id: &str, publish_rate_hz: f64) -> Result<CapabilitySchedule> {
    validate_publish_rate(capability_id, publish_rate_hz)?;
    Ok(CapabilitySchedule {
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
    use super::{Bno085, divisor_for_rate, is_due};
    use phoxal::api;
    use phoxal::bus::ContractBody;
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

    #[test]
    fn divisor_rounds_to_fixed_step_clock() {
        assert_eq!(divisor_for_rate(100.0, 100.0), 1);
        assert_eq!(divisor_for_rate(100.0, 50.0), 2);
        assert_eq!(divisor_for_rate(100.0, 20.0), 5);
        assert_eq!(divisor_for_rate(100.0, 1000.0), 1);
        assert!(is_due(10, 5));
        assert!(!is_due(11, 5));
    }

    #[test]
    fn api_reports_per_component_contracts() {
        assert_eq!(<Bno085 as Participant>::ID, "bno085");

        let contracts = <<Bno085 as Participant>::Api as ParticipantApi>::CONTRACTS;
        for family in [
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
