//! What the robot's component catalog asks this controller to simulate.
//!
//! The catalog is read once at setup and fixes the binding order for
//! everything downstream: the devices the backend opens and the bus handles
//! the controller publishes on are built from this one sequence, so a
//! capability's device and its handle are never matched up by position again.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use phoxal::model::Robot;
use phoxal::model::component::capability::{Capability, CapabilityKind};
use phoxal::model::identity::CapabilityRef;

use crate::capabilities::SampledSpec;
use crate::capabilities::battery::BatterySpec;
use crate::capabilities::camera::CameraSpec;
use crate::capabilities::depth::DepthSpec;
use crate::capabilities::encoder::EncoderSpec;
use crate::capabilities::lidar::LidarSpec;
use crate::capabilities::motor::MotorSpec;
use crate::capabilities::range::RangeSpec;

/// The step cadence declared publish rates are resolved against.
///
/// It is the controller's assumption about how fast the loop runs, not a
/// figure read from the world: the real cadence is the world's
/// `basicTimeStep`, and a world that steps at another rate publishes at a
/// correspondingly different one.
const STEP_HZ: f64 = 100.0;

// `SampleSchedule` validates the publish rate but takes the step cadence on
// trust, so the cadence this crate feeds it is proven here instead - at compile
// time, which is stronger than the runtime check it replaces.
const _: () = assert!(
    STEP_HZ > 0.0,
    "the step cadence publish rates are divided by must be positive"
);

/// One capability this controller simulates, carrying everything binding it
/// needs.
///
/// Each family keeps its own spec type: what a lidar needs to bind has nothing
/// in common with what a battery needs, and collapsing them would only move
/// the difference into a runtime check.
pub(crate) enum CapabilitySpec {
    Motor(MotorSpec),
    Encoder(EncoderSpec),
    Imu(SampledSpec),
    Accelerometer(SampledSpec),
    Gyroscope(SampledSpec),
    Range(RangeSpec),
    Camera(CameraSpec),
    Depth(DepthSpec),
    Gnss(SampledSpec),
    Magnetometer(SampledSpec),
    Lidar(LidarSpec),
    Mmwave(SampledSpec),
    Microphone(SampledSpec),
    Battery(BatterySpec),
    Led(CapabilityRef),
    Speaker(CapabilityRef),
}

impl CapabilitySpec {
    /// The capability this spec binds.
    pub(crate) fn reference(&self) -> &CapabilityRef {
        match self {
            Self::Motor(spec) => &spec.reference,
            Self::Encoder(spec) => &spec.sampled.reference,
            Self::Imu(spec)
            | Self::Accelerometer(spec)
            | Self::Gyroscope(spec)
            | Self::Gnss(spec)
            | Self::Magnetometer(spec)
            | Self::Mmwave(spec)
            | Self::Microphone(spec) => &spec.reference,
            Self::Range(spec) => &spec.sampled.reference,
            Self::Camera(spec) => &spec.sampled.reference,
            Self::Depth(spec) => &spec.sampled.reference,
            Self::Lidar(spec) => &spec.sampled.reference,
            Self::Battery(spec) => &spec.sampled.reference,
            Self::Led(reference) | Self::Speaker(reference) => reference,
        }
    }

    /// The device kind this spec binds.
    pub(crate) const fn kind(&self) -> CapabilityKind {
        match self {
            Self::Motor(_) => CapabilityKind::Motor,
            Self::Encoder(_) => CapabilityKind::Encoder,
            Self::Imu(_) => CapabilityKind::Imu,
            Self::Accelerometer(_) => CapabilityKind::Accelerometer,
            Self::Gyroscope(_) => CapabilityKind::Gyroscope,
            Self::Range(_) => CapabilityKind::Range,
            Self::Camera(_) => CapabilityKind::Camera,
            Self::Depth(_) => CapabilityKind::Depth,
            Self::Gnss(_) => CapabilityKind::Gnss,
            Self::Magnetometer(_) => CapabilityKind::Magnetometer,
            Self::Lidar(_) => CapabilityKind::Lidar,
            Self::Mmwave(_) => CapabilityKind::Mmwave,
            Self::Microphone(_) => CapabilityKind::Microphone,
            Self::Battery(_) => CapabilityKind::Battery,
            Self::Led(_) => CapabilityKind::Led,
            Self::Speaker(_) => CapabilityKind::Speaker,
        }
    }
}

/// Every capability of this robot's Webots model the controller simulates, in
/// the robot's canonical `(component id, capability id)` order.
pub(crate) struct CapabilityCatalog {
    specs: Vec<CapabilitySpec>,
}

impl CapabilityCatalog {
    /// Read the staged component catalog (`ctx.robot()`) into binding specs.
    ///
    /// # Errors
    ///
    /// Returns an error when a declared capability describes a cadence no
    /// divisor exists for, or when the robot declares more batteries than a
    /// Webots robot can hold.
    pub(crate) fn from_robot(robot: &Robot) -> Result<Self> {
        let mut specs = Vec::new();

        for reference in robot.capability_refs(|_| true) {
            let capability = robot.capability(&reference).ok_or_else(|| {
                anyhow!("robot declares capability {reference} but does not resolve it")
            })?;
            let simulated = robot
                .simulation_for_instance(reference.component_id.as_str())
                .and_then(|simulation| simulation.capability(reference.capability_id.as_str()));

            match capability {
                Capability::Motor(config) => specs.push(CapabilitySpec::Motor(MotorSpec {
                    reference,
                    actuator_type: config.command,
                    gear_ratio: config.gear_ratio,
                })),
                Capability::Encoder(config) => specs.push(CapabilitySpec::Encoder(EncoderSpec {
                    sampled: SampledSpec::new(
                        reference,
                        STEP_HZ,
                        config.publish_rate_hz,
                        simulated,
                    )?,
                    gear_ratio: config.gear_ratio,
                })),
                Capability::Imu(config) => specs.push(CapabilitySpec::Imu(SampledSpec::new(
                    reference,
                    STEP_HZ,
                    config.publish_rate_hz,
                    simulated,
                )?)),
                Capability::Accelerometer(config) => {
                    specs.push(CapabilitySpec::Accelerometer(SampledSpec::new(
                        reference,
                        STEP_HZ,
                        config.publish_rate_hz,
                        simulated,
                    )?));
                }
                Capability::Gyroscope(config) => {
                    specs.push(CapabilitySpec::Gyroscope(SampledSpec::new(
                        reference,
                        STEP_HZ,
                        config.publish_rate_hz,
                        simulated,
                    )?));
                }
                Capability::Range(config) => specs.push(CapabilitySpec::Range(RangeSpec {
                    sampled: SampledSpec::new(
                        reference,
                        STEP_HZ,
                        config.publish_rate_hz,
                        simulated,
                    )?,
                    min_range_m: config.min_range_m as f32,
                    max_range_m: config.max_range_m as f32,
                })),
                Capability::Camera(config) => specs.push(CapabilitySpec::Camera(CameraSpec {
                    sampled: SampledSpec::new(
                        reference,
                        STEP_HZ,
                        config.publish_rate_hz,
                        simulated,
                    )?,
                    mode: config.mode,
                    width: config.width_px,
                    height: config.height_px,
                })),
                Capability::Depth(config) => specs.push(CapabilitySpec::Depth(DepthSpec {
                    sampled: SampledSpec::new(
                        reference,
                        STEP_HZ,
                        config.publish_rate_hz,
                        simulated,
                    )?,
                    width: config.width_px,
                    height: config.height_px,
                })),
                Capability::Gnss(config) => specs.push(CapabilitySpec::Gnss(SampledSpec::new(
                    reference,
                    STEP_HZ,
                    config.publish_rate_hz,
                    simulated,
                )?)),
                Capability::Magnetometer(config) => {
                    specs.push(CapabilitySpec::Magnetometer(SampledSpec::new(
                        reference,
                        STEP_HZ,
                        config.publish_rate_hz,
                        simulated,
                    )?));
                }
                Capability::Lidar(config) => specs.push(CapabilitySpec::Lidar(LidarSpec {
                    sampled: SampledSpec::new(
                        reference,
                        STEP_HZ,
                        config.publish_rate_hz,
                        simulated,
                    )?,
                    output: config.output,
                })),
                Capability::Mmwave(config) => {
                    specs.push(CapabilitySpec::Mmwave(SampledSpec::new(
                        reference,
                        STEP_HZ,
                        config.publish_rate_hz,
                        simulated,
                    )?));
                }
                Capability::Microphone(config) => {
                    specs.push(CapabilitySpec::Microphone(SampledSpec::new(
                        reference,
                        STEP_HZ,
                        config.publish_rate_hz,
                        simulated,
                    )?));
                }
                Capability::Battery(config) => specs.push(CapabilitySpec::Battery(BatterySpec {
                    sampled: SampledSpec::new(
                        reference,
                        STEP_HZ,
                        config.publish_rate_hz,
                        simulated,
                    )?,
                    voltage_v: config.voltage_v,
                    capacity_ah: config.capacity_ah,
                })),
                Capability::Led(_) => specs.push(CapabilitySpec::Led(reference)),
                Capability::Speaker(_) => specs.push(CapabilitySpec::Speaker(reference)),
                // Webots has no button, switch, or toggle node, so nothing in
                // a simulated world can engage or release an e-stop. Leaving
                // it unpublished is the honest state: `motion` fails closed on
                // a component it never hears from.
                Capability::EmergencyStop(_) => {
                    tracing::debug!(
                        target: "simulator_webots_controller",
                        capability = %reference,
                        kind = %capability.kind(),
                        "Webots models no emergency-stop control, so this capability is \
                         not simulated"
                    );
                }
            }
        }

        let catalog = Self { specs };
        catalog.validate()?;
        Ok(catalog)
    }

    /// The specs to bind, in binding order.
    pub(crate) fn specs(&self) -> &[CapabilitySpec] {
        &self.specs
    }

    /// How many capabilities of each kind this robot declares, for the
    /// controller's readiness log.
    pub(crate) fn kind_counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::new();
        for spec in &self.specs {
            *counts.entry(spec.kind().as_str()).or_default() += 1;
        }
        counts
    }

    /// Webots gives a robot exactly one battery sensor, so a second battery
    /// capability would report the first one's energy under another name.
    fn validate(&self) -> Result<()> {
        let batteries = self
            .specs
            .iter()
            .filter(|spec| matches!(spec, CapabilitySpec::Battery(_)))
            .map(|spec| spec.reference().to_string())
            .collect::<Vec<_>>();
        if batteries.len() > 1 {
            bail!(
                "Webots models one battery per robot, but this robot declares {}: {}. \
                 Keep exactly one battery capability for a simulated robot.",
                batteries.len(),
                batteries.join(", ")
            );
        }
        Ok(())
    }
}
