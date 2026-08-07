//! `bno085` - BNO085 IMU component driver stub.

use anyhow::{Result, anyhow, bail};
use phoxal::SampleSchedule;
use phoxal::api;
use phoxal::model::component::capability::Capability;
use phoxal::model::identity::CapabilityId;
use phoxal::prelude::*;

/// The fixed step cadence every capability schedule below divides.
const STEP_HZ: f64 = 100.0;

pub(crate) struct Api {
    imu: Vec<MeasurementPublisher<api::component::imu::Sample>>,
    accelerometer: Vec<MeasurementPublisher<api::component::accelerometer::Sample>>,
    gyroscope: Vec<MeasurementPublisher<api::component::gyroscope::Sample>>,
}

pub(crate) struct Bno085State {
    imu_schedules: Vec<SampleSchedule>,
    accelerometer_schedules: Vec<SampleSchedule>,
    gyroscope_schedules: Vec<SampleSchedule>,
}

/// One declared capability of this instance and the cadence it publishes at.
///
/// The publishers are acquired in the order these are collected, so the
/// schedule at index `i` belongs to the publisher at index `i`.
#[derive(Debug, Clone)]
struct CapabilitySchedule {
    capability_id: CapabilityId,
    schedule: SampleSchedule,
}

impl CapabilitySchedule {
    fn new(capability_id: &CapabilityId, publish_rate_hz: f64) -> Result<Self> {
        Ok(CapabilitySchedule {
            capability_id: capability_id.clone(),
            schedule: SampleSchedule::new(capability_id.as_str(), STEP_HZ, publish_rate_hz)?,
        })
    }
}

#[phoxal::driver(state = Bno085State, api = Api)]
pub(crate) struct Bno085;

impl Participant for Bno085 {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let instance = ctx.component()?.id().clone();
        let (imu_slots, accelerometer_slots, gyroscope_slots) = {
            let robot = ctx.robot()?;
            let spec = robot
                .component_for_instance(instance.as_str())
                .ok_or_else(|| anyhow!("no component type is loaded for instance '{instance}'"))?;
            let mut imu = Vec::new();
            let mut accelerometer = Vec::new();
            let mut gyroscope = Vec::new();

            for (capability_id, capability) in spec.capabilities() {
                match capability {
                    Capability::Imu(config) => {
                        imu.push(CapabilitySchedule::new(
                            capability_id,
                            config.publish_rate_hz,
                        )?);
                    }
                    Capability::Accelerometer(config) => {
                        accelerometer.push(CapabilitySchedule::new(
                            capability_id,
                            config.publish_rate_hz,
                        )?);
                    }
                    Capability::Gyroscope(config) => {
                        gyroscope.push(CapabilitySchedule::new(
                            capability_id,
                            config.publish_rate_hz,
                        )?);
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
            Bno085State {
                imu_schedules,
                accelerometer_schedules,
                gyroscope_schedules,
            },
            Api {
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
