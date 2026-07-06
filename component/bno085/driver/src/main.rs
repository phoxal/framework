//! `bno085` - BNO085 IMU component driver stub.

use anyhow::{Result, bail};
use phoxal::model::component::v1::capability::Capability;
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

const STEP_HZ: f64 = 100.0;

#[derive(phoxal::Driver)]
#[phoxal(id = "bno085", api = y2026_1)]
struct Bno085 {
    imu: Vec<Publisher<api::component::imu::Sample>>,
    imu_divisors: Vec<u64>,
    accelerometer: Vec<Publisher<api::component::accelerometer::Sample>>,
    accelerometer_divisors: Vec<u64>,
    gyroscope: Vec<Publisher<api::component::gyroscope::Sample>>,
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
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
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
    phoxal::run::<Bno085>()
}

#[cfg(test)]
mod tests {
    use super::{Bno085, divisor_for_rate, is_due};
    use phoxal_api::ContractBody;
    use phoxal_api::y2026_1 as api;

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
    fn emit_apis_reports_per_component_contracts() {
        let json = phoxal::participant::emit_apis_json::<Bno085>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "bno085");
        assert_eq!(value["artifact"]["kind"], "driver");
        assert_eq!(value["participant_class"], "checked");
        let contracts = value["required_contracts"].as_array().unwrap();
        assert!(
            contracts.iter().all(|c| c["schema_id"]
                .as_str()
                .is_some_and(|schema_id| !schema_id.is_empty())),
            "each contract should include schema_id"
        );
        for family in [
            <api::component::imu::Sample as ContractBody>::FAMILY,
            <api::component::accelerometer::Sample as ContractBody>::FAMILY,
            <api::component::gyroscope::Sample as ContractBody>::FAMILY,
        ] {
            assert!(
                contracts
                    .iter()
                    .any(|c| c["family"] == family && c["direction"] == "publish"),
                "missing publish contract for {family}"
            );
        }
    }
}
