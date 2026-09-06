//! Shared typed planning for one simulated robot.
//!
//! The compiled robot model and its asset resolver remain authoritative.
//! A plan is a deterministic derived value shared by early preflight, host admission, native
//! generation, and the controller. No Webots scene mutation is allowed before this derivation has
//! validated the entire robot.

use std::collections::BTreeMap;

use phoxal::model::Robot;
use phoxal::model::asset::AssetId;
use phoxal::model::component::capability::{
    Capability as DeclaredCapability, CapabilityKind, GnssCoordinateSystem, MotorCommand,
    StructuralTarget,
};
use phoxal::model::identity::{CapabilityRef, ComponentInstanceId};
use phoxal::model::simulation::{
    ActuatorType, Capability as SimulatedCapability, FullSimulationError, FullSimulationPlan,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const NANOS_PER_SECOND: f64 = 1_000_000_000.0;

/// The all-or-nothing derived plan for one robot import.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotSimulationPlan {
    pub robot: String,
    pub basic_time_step_ms: i32,
    pub substitutions: Vec<DriverSubstitution>,
    pub capabilities: Vec<CapabilityBinding>,
    pub links: Vec<LinkSimulation>,
    pub assets: Vec<PlannedAsset>,
}

/// One real driver participant omitted and replaced by the per-Robot controller.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverSubstitution {
    pub participant: ComponentInstanceId,
    pub capabilities: Vec<CapabilityRef>,
}

/// One typed capability bound to one unique native device.
///
/// The variants make native I/O obligations explicit. A motor cannot carry a
/// sampling cadence, and a sampled device cannot accidentally acquire a motor
/// command contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CapabilityBinding {
    Motor {
        reference: CapabilityRef,
        native_device: String,
        target: PlannedTarget,
        command: MotorCommand,
    },
    Encoder {
        reference: CapabilityRef,
        native_device: String,
        target: PlannedTarget,
        sampling: SamplingPlan,
    },
    Sampled {
        reference: CapabilityRef,
        native_device: String,
        target: PlannedTarget,
        capability: SampledCapabilityKind,
        sampling: SamplingPlan,
    },
}

impl CapabilityBinding {
    #[must_use]
    pub fn reference(&self) -> &CapabilityRef {
        match self {
            Self::Motor { reference, .. }
            | Self::Encoder { reference, .. }
            | Self::Sampled { reference, .. } => reference,
        }
    }

    #[must_use]
    pub fn native_device(&self) -> &str {
        match self {
            Self::Motor { native_device, .. }
            | Self::Encoder { native_device, .. }
            | Self::Sampled { native_device, .. } => native_device,
        }
    }

    #[must_use]
    pub fn target(&self) -> &PlannedTarget {
        match self {
            Self::Motor { target, .. }
            | Self::Encoder { target, .. }
            | Self::Sampled { target, .. } => target,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> CapabilityKind {
        match self {
            Self::Motor { .. } => CapabilityKind::Motor,
            Self::Encoder { .. } => CapabilityKind::Encoder,
            Self::Sampled { capability, .. } => capability.capability_kind(),
        }
    }

    #[must_use]
    pub fn sampling(&self) -> Option<&SamplingPlan> {
        match self {
            Self::Encoder { sampling, .. } | Self::Sampled { sampling, .. } => Some(sampling),
            Self::Motor { .. } => None,
        }
    }

    #[must_use]
    pub const fn motor_command(&self) -> Option<MotorCommand> {
        match self {
            Self::Motor { command, .. } => Some(*command),
            Self::Encoder { .. } | Self::Sampled { .. } => None,
        }
    }
}

/// Capability kinds supported by Webots' sampled native device path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampledCapabilityKind {
    Accelerometer,
    Gyroscope,
    Imu,
    Gnss,
    Camera,
    Depth,
    Range,
}

impl SampledCapabilityKind {
    #[must_use]
    pub const fn capability_kind(self) -> CapabilityKind {
        match self {
            Self::Accelerometer => CapabilityKind::Accelerometer,
            Self::Gyroscope => CapabilityKind::Gyroscope,
            Self::Imu => CapabilityKind::Imu,
            Self::Gnss => CapabilityKind::Gnss,
            Self::Camera => CapabilityKind::Camera,
            Self::Depth => CapabilityKind::Depth,
            Self::Range => CapabilityKind::Range,
        }
    }

    fn from_capability_kind(kind: CapabilityKind) -> Option<Self> {
        Some(match kind {
            CapabilityKind::Accelerometer => Self::Accelerometer,
            CapabilityKind::Gyroscope => Self::Gyroscope,
            CapabilityKind::Imu => Self::Imu,
            CapabilityKind::Gnss => Self::Gnss,
            CapabilityKind::Camera => Self::Camera,
            CapabilityKind::Depth => Self::Depth,
            CapabilityKind::Range => Self::Range,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlannedTarget {
    Link { id: String },
    Joint { id: String },
}

/// Deterministic cadence after quantization to the world's Webots step grid.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplingPlan {
    pub publish_rate_hz: f64,
    pub native_sampling_rate_hz: f64,
    pub native_period_ms: i32,
    pub publish_period_ns: u64,
}

/// One simulated link fact preserved for native generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkSimulation {
    pub component: ComponentInstanceId,
    pub link: String,
    pub contact_material: Option<String>,
}

/// One asset proven reachable during planning.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedAsset {
    pub id: AssetId,
    pub bytes: u64,
    pub sha256: String,
}

/// A complete plan could not be derived before native mutation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PlanError {
    #[error("Webots basicTimeStep must be a positive whole millisecond")]
    InvalidTimeStep,
    #[error("Webots does not support {kind} capability '{capability}': {detail}")]
    UnsupportedCapability {
        capability: String,
        kind: String,
        detail: String,
    },
    #[error(
        "motor '{capability}' declares {declared:?}, but simulation config requests {simulated:?}"
    )]
    ActuationMismatch {
        capability: String,
        declared: MotorCommand,
        simulated: ActuatorType,
    },
    #[error("capability '{capability}' has invalid cadence: {detail}")]
    InvalidCadence { capability: String, detail: String },
    #[error(transparent)]
    DriveAuthority(#[from] phoxal::simulator::DriveAuthorityError),
    #[error("native Webots device name '{device}' is claimed by both {first} and {second}")]
    DuplicateDevice {
        device: String,
        first: String,
        second: String,
    },
    #[error("required simulation asset '{asset}' is unavailable: {detail}")]
    MissingAsset { asset: String, detail: String },
    #[error("required simulation asset '{asset}' is empty")]
    EmptyAsset { asset: String },
    #[error(transparent)]
    FullSimulation(#[from] FullSimulationError),
}

impl RobotSimulationPlan {
    /// Enumerate the complete deterministic asset closure for asynchronous prefetch.
    pub fn required_assets(robot: &Robot) -> Result<Vec<AssetId>, PlanError> {
        Ok(FullSimulationPlan::derive(robot)?
            .required_assets()
            .cloned()
            .collect())
    }

    /// Derive and validate the complete plan, including every asset byte, before mutation.
    pub fn derive<F, E>(
        robot: &Robot,
        basic_time_step_ms: i32,
        mut resolve_asset: F,
    ) -> Result<Self, PlanError>
    where
        F: FnMut(&AssetId) -> Result<Vec<u8>, E>,
        E: std::fmt::Display,
    {
        if basic_time_step_ms <= 0 {
            return Err(PlanError::InvalidTimeStep);
        }
        let full_plan = FullSimulationPlan::derive(robot)?;
        let substitutions = full_plan
            .substitutions()
            .map(|substitution| DriverSubstitution {
                participant: substitution.participant().clone(),
                capabilities: substitution.capabilities().cloned().collect(),
            })
            .collect();
        let mut capabilities = Vec::new();
        let mut links = Vec::new();
        let mut claimed_devices = BTreeMap::<String, String>::new();
        let asset_ids = full_plan.required_assets();

        for component in robot.components() {
            let component_id = component.id();
            let declared = component.component_type();
            let simulation = component.simulation().ok_or_else(|| {
                PlanError::FullSimulation(FullSimulationError::MissingSimulation {
                    component: component_id.clone(),
                })
            })?;

            for (capability_id, capability) in declared.capabilities() {
                let reference = CapabilityRef::new(component_id.clone(), capability_id.clone());
                let simulated = simulation
                    .capability(capability_id.as_str())
                    .ok_or_else(|| {
                        PlanError::FullSimulation(FullSimulationError::MissingCapability {
                            capability: reference.clone(),
                        })
                    })?;
                validate_supported_facts(&reference, capability, simulated)?;
                let binding =
                    bind_capability(reference.clone(), capability, simulated, basic_time_step_ms)?;
                if matches!(capability, DeclaredCapability::Motor(_)) {
                    phoxal::simulator::DriveCommandAuthority::validate_motor(robot, &reference)?;
                }
                match capability.target() {
                    StructuralTarget::Joint { id } => {
                        let joint = declared.structure().joint(id.as_str()).ok_or_else(|| {
                            PlanError::UnsupportedCapability {
                                capability: reference.to_string(),
                                kind: capability.kind().to_string(),
                                detail: format!(
                                    "target joint '{id}' is absent from component structure {}",
                                    component_id
                                ),
                            }
                        })?;
                        if !matches!(
                            joint.kind(),
                            phoxal::model::structure::JointKind::Revolute
                                | phoxal::model::structure::JointKind::Continuous
                                | phoxal::model::structure::JointKind::Prismatic
                        ) {
                            return Err(PlanError::UnsupportedCapability {
                                capability: reference.to_string(),
                                kind: capability.kind().to_string(),
                                detail: format!(
                                    "target joint '{id}' has unsupported {:?} native kind",
                                    joint.kind()
                                ),
                            });
                        }
                        if matches!(
                            capability.kind(),
                            CapabilityKind::Motor | CapabilityKind::Encoder
                        ) && joint.kind() == phoxal::model::structure::JointKind::Prismatic
                        {
                            return Err(PlanError::UnsupportedCapability {
                                capability: reference.to_string(),
                                kind: capability.kind().to_string(),
                                detail: "the current typed motor and encoder contracts use rotational units and cannot bind a linear Webots joint".to_owned(),
                            });
                        }
                    }
                    StructuralTarget::Link { id } => {
                        if declared.structure().link(id.as_str()).is_none() {
                            return Err(PlanError::UnsupportedCapability {
                                capability: reference.to_string(),
                                kind: capability.kind().to_string(),
                                detail: format!(
                                    "target link '{id}' is absent from component structure {}",
                                    component_id
                                ),
                            });
                        }
                    }
                }
                for device in native_device_names(&binding) {
                    if let Some(previous) =
                        claimed_devices.insert(device.clone(), reference.to_string())
                    {
                        return Err(PlanError::DuplicateDevice {
                            device,
                            first: previous,
                            second: reference.to_string(),
                        });
                    }
                }
                capabilities.push(binding);
            }
            for (link, config) in simulation.links() {
                let structure = declared.structure();
                if structure.link(link.as_str()).is_none() {
                    return Err(PlanError::UnsupportedCapability {
                        capability: component_id.to_string(),
                        kind: "link_simulation".to_owned(),
                        detail: format!(
                            "simulation link '{link}' is absent from the component structure"
                        ),
                    });
                }
                if config.contact_material().is_some()
                    && !has_movable_parent(structure, link.as_str())
                {
                    return Err(PlanError::UnsupportedCapability {
                        capability: component_id.to_string(),
                        kind: "contact_material".to_owned(),
                        detail: format!(
                            "contact material on rigidly mounted link '{link}' cannot be represented independently after fixed-body aggregation"
                        ),
                    });
                }
                links.push(LinkSimulation {
                    component: component_id.clone(),
                    link: link.as_str().to_owned(),
                    contact_material: config.contact_material().map(str::to_owned),
                });
            }
        }

        let mut assets = Vec::with_capacity(asset_ids.len());
        for id in asset_ids {
            let bytes = resolve_asset(id).map_err(|error| PlanError::MissingAsset {
                asset: id.to_string(),
                detail: error.to_string(),
            })?;
            if bytes.is_empty() {
                return Err(PlanError::EmptyAsset {
                    asset: id.to_string(),
                });
            }
            assets.push(PlannedAsset {
                id: id.clone(),
                bytes: u64::try_from(bytes.len()).map_err(|_| PlanError::MissingAsset {
                    asset: "asset larger than addressable memory".to_owned(),
                    detail: "length does not fit u64".to_owned(),
                })?,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
            });
        }

        Ok(Self {
            robot: robot.id().to_string(),
            basic_time_step_ms,
            substitutions,
            capabilities,
            links,
            assets,
        })
    }
}

fn validate_supported_facts(
    reference: &CapabilityRef,
    declared: &DeclaredCapability,
    simulated: &SimulatedCapability,
) -> Result<(), PlanError> {
    let unsupported = |detail: String| PlanError::UnsupportedCapability {
        capability: reference.to_string(),
        kind: declared.kind().to_string(),
        detail,
    };
    if let SimulatedCapability::Motor(config) = simulated {
        if config.sampling_period_torque_hz.is_some() {
            return Err(unsupported(
                "torque-feedback sampling has no typed publication path in the v0 adapter"
                    .to_owned(),
            ));
        }
        if let Some(pid) = &config.control_pid
            && pid.len() != 3
        {
            return Err(unsupported(
                "Webots control_pid must contain exactly P, I, and D".to_owned(),
            ));
        }
    }
    Ok(())
}

fn has_movable_parent(structure: &phoxal::model::structure::Structure, link: &str) -> bool {
    structure
        .parent_joint(link)
        .is_some_and(|joint| joint.kind() != phoxal::model::structure::JointKind::Fixed)
}

fn bind_capability(
    reference: CapabilityRef,
    declared: &DeclaredCapability,
    simulated: &SimulatedCapability,
    basic_time_step_ms: i32,
) -> Result<CapabilityBinding, PlanError> {
    let kind = declared.kind();
    if !matches!(
        kind,
        CapabilityKind::Motor
            | CapabilityKind::Encoder
            | CapabilityKind::Accelerometer
            | CapabilityKind::Gyroscope
            | CapabilityKind::Imu
            | CapabilityKind::Gnss
            | CapabilityKind::Camera
            | CapabilityKind::Depth
            | CapabilityKind::Range
    ) {
        return Err(PlanError::UnsupportedCapability {
            capability: reference.to_string(),
            kind: kind.to_string(),
            detail: "the R2025a adapter currently has no complete native generation and typed I/O path for this capability".to_owned(),
        });
    }
    if let DeclaredCapability::Gnss(config) = declared
        && config.coordinate_system != GnssCoordinateSystem::Wgs84
    {
        return Err(PlanError::UnsupportedCapability {
            capability: reference.to_string(),
            kind: kind.to_string(),
            detail: "the typed GNSS sample is geographic, so Webots admission requires an explicit wgs84 coordinate system".to_owned(),
        });
    }
    let native_device = match kind {
        CapabilityKind::Battery => "__phoxal_battery".to_owned(),
        _ => reference.to_string(),
    };
    let joint_device = matches!(kind, CapabilityKind::Motor | CapabilityKind::Encoder);
    if joint_device != matches!(declared.target(), StructuralTarget::Joint { .. }) {
        return Err(PlanError::UnsupportedCapability {
            capability: reference.to_string(),
            kind: kind.to_string(),
            detail: if joint_device {
                "Webots motor and encoder devices must target a movable joint"
            } else {
                "Webots sampled body devices must target a link"
            }
            .to_owned(),
        });
    }
    let target = match declared.target().namespaced(&reference.component_id) {
        StructuralTarget::Link { id } => PlannedTarget::Link { id: id.to_string() },
        StructuralTarget::Joint { id } => PlannedTarget::Joint { id: id.to_string() },
    };
    let sampling = sampling_rates(declared, simulated)
        .map(|(publish, native)| sampling_plan(&reference, basic_time_step_ms, publish, native))
        .transpose()?;
    let motor_command = match (declared, simulated) {
        (DeclaredCapability::Motor(config), SimulatedCapability::Motor(native)) => {
            let expected = match config.command {
                MotorCommand::Position => ActuatorType::Position,
                MotorCommand::Velocity => ActuatorType::Velocity,
                MotorCommand::Torque => ActuatorType::Torque,
            };
            if native.actuator_type != expected {
                return Err(PlanError::ActuationMismatch {
                    capability: reference.to_string(),
                    declared: config.command,
                    simulated: native.actuator_type,
                });
            }
            Some(config.command)
        }
        _ => None,
    };
    let capability_name = reference.to_string();
    let kind_name = kind.to_string();
    let incomplete = || PlanError::UnsupportedCapability {
        capability: capability_name.clone(),
        kind: kind_name.clone(),
        detail: "the compiled native I/O contract is incomplete for this capability".to_owned(),
    };
    match (kind, motor_command, sampling) {
        (CapabilityKind::Motor, Some(command), None) => Ok(CapabilityBinding::Motor {
            reference,
            native_device,
            target,
            command,
        }),
        (CapabilityKind::Encoder, None, Some(sampling)) => Ok(CapabilityBinding::Encoder {
            reference,
            native_device,
            target,
            sampling,
        }),
        (kind, None, Some(sampling)) => {
            let capability =
                SampledCapabilityKind::from_capability_kind(kind).ok_or_else(incomplete)?;
            Ok(CapabilityBinding::Sampled {
                reference,
                native_device,
                target,
                capability,
                sampling,
            })
        }
        _ => Err(incomplete()),
    }
}

fn native_device_names(binding: &CapabilityBinding) -> Vec<String> {
    let mut devices = vec![binding.native_device().to_owned()];
    if binding.kind() == CapabilityKind::Imu {
        devices.push(format!("{}__accel", binding.native_device()));
        devices.push(format!("{}__gyro", binding.native_device()));
    }
    devices
}

fn sampling_rates(
    declared: &DeclaredCapability,
    simulated: &SimulatedCapability,
) -> Option<(f64, f64)> {
    Some(match (declared, simulated) {
        (DeclaredCapability::Encoder(a), SimulatedCapability::Encoder(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Accelerometer(a), SimulatedCapability::Accelerometer(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Gyroscope(a), SimulatedCapability::Gyroscope(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Magnetometer(a), SimulatedCapability::Magnetometer(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Imu(a), SimulatedCapability::Imu(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Gnss(a), SimulatedCapability::Gnss(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Camera(a), SimulatedCapability::Camera(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Depth(a), SimulatedCapability::Depth(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Range(a), SimulatedCapability::Range(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Lidar(a), SimulatedCapability::Lidar(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Mmwave(a), SimulatedCapability::Mmwave(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Microphone(a), SimulatedCapability::Microphone(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Battery(a), SimulatedCapability::Battery) => {
            (a.publish_rate_hz, a.publish_rate_hz)
        }
        _ => return None,
    })
}

fn sampling_plan(
    reference: &CapabilityRef,
    basic_time_step_ms: i32,
    publish_rate_hz: f64,
    native_rate_hz: f64,
) -> Result<SamplingPlan, PlanError> {
    let failure = |detail: &str| PlanError::InvalidCadence {
        capability: reference.to_string(),
        detail: detail.to_owned(),
    };
    if !publish_rate_hz.is_finite() || publish_rate_hz <= 0.0 {
        return Err(failure("publish rate must be finite and positive"));
    }
    if !native_rate_hz.is_finite() || native_rate_hz <= 0.0 {
        return Err(failure("native sampling rate must be finite and positive"));
    }
    let requested_ms = (1000.0 / native_rate_hz).round().max(1.0);
    if requested_ms > f64::from(i32::MAX) {
        return Err(failure("native sampling period exceeds Webots range"));
    }
    let requested = u64::try_from(requested_ms as i64)
        .map_err(|_| failure("native sampling period is invalid"))?;
    let basic = u64::try_from(basic_time_step_ms).map_err(|_| failure("world step is invalid"))?;
    let steps = requested
        .checked_add(basic - 1)
        .and_then(|value| value.checked_div(basic))
        .ok_or_else(|| failure("native sampling quantization overflowed"))?;
    let native_period = steps
        .checked_mul(basic)
        .ok_or_else(|| failure("native sampling quantization overflowed"))?;
    let publish_period = (NANOS_PER_SECOND / publish_rate_hz).round();
    if !(1.0..=u64::MAX as f64).contains(&publish_period) {
        return Err(failure("publish period does not fit nanoseconds"));
    }
    Ok(SamplingPlan {
        publish_rate_hz,
        native_sampling_rate_hz: native_rate_hz,
        native_period_ms: i32::try_from(native_period)
            .map_err(|_| failure("native sampling period exceeds Webots range"))?,
        publish_period_ns: publish_period as u64,
    })
}

#[cfg(test)]
mod tests {
    use phoxal::model::builder::{Joint, RobotBuilder};
    use phoxal::model::simulation;
    use phoxal::model::structure::JointKind;

    use super::*;

    #[test]
    fn complete_mapping_produces_deterministic_quantized_bindings() {
        let robot = RobotBuilder::new("rover")
            .component_type("wheel", |builder| {
                builder.encoder("encoder", "axle").simulated(
                    "encoder",
                    simulation::Capability::Encoder(simulation::Encoder {
                        sampling_period_hz: 62.5,
                        ..Default::default()
                    }),
                )
            })
            .component("left", "wheel")
            .build()
            .expect("robot is valid");
        let first = RobotSimulationPlan::derive(&robot, 12, |_id| {
            Result::<Vec<u8>, &'static str>::Ok(vec![1])
        })
        .expect("plan is complete");
        let second = RobotSimulationPlan::derive(&robot, 12, |_id| {
            Result::<Vec<u8>, &'static str>::Ok(vec![1])
        })
        .expect("plan is repeatable");
        assert_eq!(first, second);
        assert_eq!(
            first.capabilities[0]
                .sampling()
                .expect("sampled")
                .native_period_ms,
            24
        );
    }

    #[test]
    fn missing_simulation_and_unsupported_effects_fail_before_mutation() {
        let missing = RobotBuilder::new("missing")
            .component_type("wheel", |builder| builder.encoder("encoder", "axle"))
            .component("left", "wheel")
            .build()
            .expect("hardware model is valid");
        assert!(matches!(
            RobotSimulationPlan::derive(&missing, 12, |_id| {
                Result::<Vec<u8>, &'static str>::Ok(vec![1])
            }),
            Err(PlanError::FullSimulation(
                FullSimulationError::MissingSimulation { .. }
            ))
        ));

        let led = RobotBuilder::new("led")
            .component_type("panel", |builder| {
                builder
                    .led("status", "base_link")
                    .simulated("status", simulation::Capability::Led)
            })
            .component("panel", "panel")
            .build()
            .expect("hardware model is valid");
        assert!(matches!(
            RobotSimulationPlan::derive(&led, 12, |_id| {
                Result::<Vec<u8>, &'static str>::Ok(vec![1])
            }),
            Err(PlanError::UnsupportedCapability { .. })
        ));
    }

    #[test]
    fn contact_material_on_fixed_descendant_is_rejected_before_rendering() {
        let robot = RobotBuilder::new("carrier")
            .component_type("wheel", |builder| {
                builder
                    .joint(Joint {
                        name: "spin",
                        kind: JointKind::Continuous,
                        parent: "mount",
                        child: "rotor",
                        ..Joint::default()
                    })
                    .joint(Joint {
                        name: "tread_joint",
                        kind: JointKind::Fixed,
                        parent: "rotor",
                        child: "tread",
                        ..Joint::default()
                    })
                    .contact_material("tread", "rubber")
            })
            .component("left", "wheel")
            .build()
            .expect("robot");

        assert!(matches!(
            RobotSimulationPlan::derive(&robot, 12, |_id| {
                Result::<Vec<u8>, &'static str>::Ok(vec![1])
            }),
            Err(PlanError::UnsupportedCapability { kind, .. }) if kind == "contact_material"
        ));
    }
}
